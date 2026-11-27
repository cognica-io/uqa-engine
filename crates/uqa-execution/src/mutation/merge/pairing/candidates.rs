//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Use positional equality for repeated MERGE probes. Ordered evaluation owns all other predicates.

use crate::distinct::{encode_typed_non_null_key, EncodedKey};
use crate::join::{simple_key_positions, HybridHashIndex, HybridRowStore};
use crate::physical::physical_exec_error;
use crate::query::sources::join_keys::join_keys;
use crate::{OwnedPhysicalRow, RowSchema, SharedSpill};
use uqa_core::{
    memory::{OwnedProductionControl, Produced},
    Value,
};
use uqa_sql::{
    ast::{BinaryOp, ComparisonOperator, JoinKind},
    semantics::volatility::VolatilityCatalog,
    ColumnType, FunctionTypeResolver, SQLError, SQLParam, ScalarExpr,
};

pub(super) struct MergeCandidates<'a> {
    condition: &'a ScalarExpr,
    params: &'a [SQLParam],
    types: &'a dyn FunctionTypeResolver,
    volatility: &'a dyn VolatilityCatalog,
    resources: OwnedProductionControl,
    budget: usize,
    access: SourceAccess,
}

enum SourceAccess {
    FirstTarget,
    Scanned(RowSchema),
    Sequential,
    Indexed(Box<IndexedSources>),
}

struct IndexedSources {
    schema: RowSchema,
    positions: Produced<Vec<usize>>,
    rows: HybridRowStore,
    index: HybridHashIndex,
}

impl<'a> MergeCandidates<'a> {
    pub(super) fn new(
        condition: &'a ScalarExpr,
        params: &'a [SQLParam],
        types: &'a dyn FunctionTypeResolver,
        volatility: &'a dyn VolatilityCatalog,
        budget: usize,
        resources: OwnedProductionControl,
    ) -> Self {
        Self {
            condition,
            params,
            types,
            volatility,
            resources,
            budget,
            access: SourceAccess::FirstTarget,
        }
    }

    pub(super) fn visit(
        &mut self,
        target: &OwnedPhysicalRow,
        source: &SharedSpill,
        mut visit: impl FnMut(u64, &OwnedPhysicalRow) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if source.rows() == 0 {
            return Ok(());
        }
        // Finish one ordinary scan before indexing repeated probes. Empty and single-target MERGE execute in their original order without building a second source store.
        match &self.access {
            SourceAccess::Scanned(schema) if schema == &target.schema => {
                self.access = self
                    .build(&target.schema, source)?
                    .map_or(SourceAccess::Sequential, |index| {
                        SourceAccess::Indexed(Box::new(index))
                    });
            }
            SourceAccess::FirstTarget | SourceAccess::Scanned(_) => {
                self.access = SourceAccess::Scanned(target.schema.clone());
            }
            _ => {}
        }
        if let SourceAccess::Indexed(input) = &mut self.access {
            if input.schema == target.schema {
                match row_key(target, &input.positions)? {
                    RowKey::Null => return Ok(()),
                    RowKey::Key(key) => {
                        let mut positions =
                            input.index.match_cursor(key).map_err(physical_exec_error)?;
                        while let Some(ordinal) = positions.next().map_err(physical_exec_error)? {
                            self.resources.control().check()?;
                            input
                                .rows
                                .with_row(ordinal, |row| {
                                    Ok(visit(
                                        ordinal,
                                        &OwnedPhysicalRow::new(
                                            source.row_schema().clone(),
                                            row.clone(),
                                        ),
                                    )?)
                                })
                                .map_err(physical_exec_error)?;
                        }
                        return Ok(());
                    }
                    RowKey::Evaluate => {}
                }
            }
        }
        for (index, source) in source.read_rows().map_err(physical_exec_error)?.enumerate() {
            self.resources.control().check()?;
            visit(index as u64, &source.map_err(physical_exec_error)?)?;
        }
        Ok(())
    }

    fn build(
        &self,
        target: &RowSchema,
        source: &SharedSpill,
    ) -> Result<Option<IndexedSources>, SQLError> {
        let control = self.resources.control();
        // Skipping a nonmatch must not suppress a volatile call, a cast error or a preceding qualification.
        let mut only_columns = true;
        uqa_sql::semantics::join_predicates::visit_join_conjuncts_with_control(
            self.condition,
            &control,
            &mut |clause| {
                let column = |value: &ScalarExpr| {
                    matches!(
                        value,
                        ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. }
                    )
                };
                only_columns &= matches!(
                    clause,
                    ScalarExpr::Binary {
                        op: BinaryOp::Equal | BinaryOp::Compare { operator: ComparisonOperator::Equal, .. },
                        lhs,
                        rhs,
                    } if column(lhs) && column(rhs)
                );
                Ok(())
            },
        )?;
        if !only_columns {
            return Ok(None);
        }
        let Some(keys) = join_keys(
            JoinKind::Inner,
            Some(self.condition),
            target,
            source.row_schema(),
            self.params,
            self.types,
            self.volatility,
            &control,
        )?
        else {
            return Ok(None);
        };
        if !keys.indeterminate.is_empty() || keys.residual.is_some() {
            return Ok(None);
        }
        let (Some(left), Some(right)) = (
            simple_key_positions(target, &keys.left, &control)?,
            simple_key_positions(source.row_schema(), &keys.right, &control)?,
        ) else {
            return Ok(None);
        };
        let row_budget = self.budget / 2;
        let mut rows = HybridRowStore::new(source.row_schema().clone(), row_budget);
        let mut index = HybridHashIndex::new(self.budget - row_budget);
        for (ordinal, row) in source.read_rows().map_err(physical_exec_error)?.enumerate() {
            control.check()?;
            let row = row.map_err(physical_exec_error)?;
            match row_key(&row, &right)? {
                RowKey::Key(key) => index
                    .insert(key, ordinal as u64)
                    .map_err(physical_exec_error)?,
                RowKey::Null => {}
                RowKey::Evaluate => return Ok(None),
            }
            rows.push(row.row).map_err(physical_exec_error)?;
        }
        Ok(Some(IndexedSources {
            schema: target.clone(),
            positions: left,
            rows,
            index,
        }))
    }
}

enum RowKey {
    Null,
    Key(EncodedKey),
    Evaluate,
}

fn row_key(row: &OwnedPhysicalRow, positions: &[usize]) -> Result<RowKey, SQLError> {
    let view = row.view();
    for &position in positions {
        let value = view.value_at(position);
        let ty = row.schema.column_type(position);
        // These SQL carriers have total native equality. Retained datums, runtime-typed rows, nested values and catalog-selected comparison procedures must preserve their original evaluation site.
        let total = matches!(
            (ty, value),
            (_, Some(Value::Null))
                | (
                    Some(
                        ColumnType::SmallInteger
                            | ColumnType::Integer
                            | ColumnType::BigInteger
                            | ColumnType::Oid
                            | ColumnType::Xid
                    ),
                    Some(Value::Int(_))
                )
                | (
                    Some(ColumnType::Real | ColumnType::DoublePrecision),
                    Some(Value::Float(_))
                )
                | (Some(ColumnType::Numeric { .. }), Some(Value::Decimal(_)))
                | (
                    Some(
                        ColumnType::Text
                            | ColumnType::Varchar(_)
                            | ColumnType::Name
                            | ColumnType::RefCursor
                            | ColumnType::Uuid
                    ),
                    Some(Value::Str(_))
                )
                | (
                    Some(ColumnType::Bpchar | ColumnType::Character(_)),
                    Some(Value::FixedChar(_))
                )
                | (Some(ColumnType::Boolean), Some(Value::Bool(_)))
                | (Some(ColumnType::Bytea), Some(Value::Bytes(_)))
        );
        if !total {
            return Ok(RowKey::Evaluate);
        }
    }
    let key = encode_typed_non_null_key(
        positions
            .iter()
            .map(|position| (view.value_at(*position), row.schema.column_type(*position))),
        &[],
        None,
    )
    .map_err(physical_exec_error)?;
    Ok(key.map_or(RowKey::Null, RowKey::Key))
}

#[cfg(test)]
mod tests;
