//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-oriented rewrite-rule action binding and internal OLD/NEW row sources.

use std::collections::BTreeMap;

use crate::ast::{
    ColumnType, Expr, FromClause, InsertStmt, InternalColumnRef, InternalRelationId, JoinKind,
    OnConflict, Projection, ReturningAliases, SelectStmt, Statement,
};
use crate::plpgsql::{bind_expr, bind_select, ResolvedVariable, VariableResolver};
use crate::SQLError;
use uqa_core::Value;

use super::{RuleColumnMetadata, RuleRowValues, RuntimeRuleResolver};

#[cfg(test)]
mod tests;

mod retained;
pub use retained::RetainedRuleInput;

type BindRuleAction<'a> = dyn Fn(&mut dyn VariableResolver) -> Result<Statement, SQLError> + 'a;

pub fn bind_insert_values_action(
    matching_rows: &[usize],
    rows: &[impl RuleRowValues],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    bind_action: &BindRuleAction<'_>,
) -> Result<Statement, SQLError> {
    bind_insert_values_action_from_rows(
        matching_rows.iter().map(|index| {
            rows.get(*index).ok_or_else(|| {
                SQLError::Internal("rewrite rule lost its qualified row image".into())
            })
        }),
        columns,
        bind_action,
    )
}

pub fn bind_insert_values_action_from_rows<R: RuleRowValues>(
    rows: impl IntoIterator<Item = Result<R, SQLError>>,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    bind_action: &BindRuleAction<'_>,
) -> Result<Statement, SQLError> {
    bind_insert_values_action_from_rows_with_storage(rows, columns, bind_action, None)
}

pub fn bind_insert_values_action_from_rows_with_storage<R: RuleRowValues>(
    rows: impl IntoIterator<Item = Result<R, SQLError>>,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    bind_action: &BindRuleAction<'_>,
    storage: Option<&dyn crate::ir::ConstantValueStorage>,
) -> Result<Statement, SQLError> {
    let mut combined = None;
    for row in rows {
        let row = row?;
        let mut resolver = runtime_rule_resolver(&row, columns);
        let bound = if let Some(storage) = storage {
            bind_action(&mut RetainedInputResolver {
                inner: &mut resolver,
                storage,
            })?
        } else {
            bind_action(&mut resolver)?
        };
        let Statement::Insert(mut insert) = bound else {
            return Err(SQLError::Internal(
                "rewrite rule INSERT VALUES action changed statement kind".into(),
            ));
        };
        if let Some(Statement::Insert(existing)) = combined.as_mut() {
            if BoundInsertContract::from(&*existing) != BoundInsertContract::from(&insert) {
                return Err(SQLError::Internal(
                    "rewrite rule INSERT VALUES action produced row-dependent statement clauses"
                        .into(),
                ));
            }
            existing.rows.append(&mut insert.rows);
        } else {
            combined = Some(Statement::Insert(insert));
        }
    }
    if let Some(bound) = combined {
        Ok(bound)
    } else {
        let mut bound = bind_action(&mut RuntimeRuleResolver {
            old: None,
            new: None,
            old_doc_id: None,
            new_doc_id: None,
            columns,
        })?;
        let Statement::Insert(insert) = &mut bound else {
            return Err(SQLError::Internal(
                "rewrite rule INSERT VALUES action changed statement kind".into(),
            ));
        };
        insert.rows.clear();
        Ok(bound)
    }
}

struct RetainedInputResolver<'a, 'b> {
    inner: &'a mut RuntimeRuleResolver<'b>,
    storage: &'a dyn crate::ir::ConstantValueStorage,
}

impl VariableResolver for RetainedInputResolver<'_, '_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_name(name)
    }
    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_qualified(qualifier, column)
    }
    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_param(index)
    }
    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        self.inner
            .rewrite_name(name)?
            .map(|value| crate::ast::RetainedInputLiteral::retain(value, self.storage))
            .transpose()
    }
    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        self.inner
            .rewrite_qualified(qualifier, column)?
            .map(|value| crate::ast::RetainedInputLiteral::retain(value, self.storage))
            .transpose()
    }
    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.inner.rewrite_qualified_whole_row(qualifier)
    }
}

#[derive(PartialEq)]
struct BoundInsertContract<'a> {
    table: &'a str,
    target_relation_bound: bool,
    target_qualifier: &'a str,
    include_descendants: bool,
    columns: &'a [crate::ast::AssignmentTarget],
    with: &'a [crate::ast::CTE],
    select_source: Option<&'a SelectStmt>,
    on_conflict: Option<&'a OnConflict>,
    returning: &'a [Projection],
    returning_aliases: &'a ReturningAliases,
}

impl<'a> From<&'a InsertStmt> for BoundInsertContract<'a> {
    fn from(insert: &'a InsertStmt) -> Self {
        Self {
            table: &insert.table,
            target_relation_bound: insert.target_relation_bound,
            target_qualifier: &insert.target_qualifier,
            include_descendants: insert.include_descendants,
            columns: &insert.columns,
            with: &insert.with,
            select_source: insert.select_source.as_deref(),
            on_conflict: insert.on_conflict.as_ref(),
            returning: &insert.returning,
            returning_aliases: &insert.returning_aliases,
        }
    }
}

fn runtime_rule_resolver<'a>(
    row: &'a dyn RuleRowValues,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
) -> RuntimeRuleResolver<'a> {
    RuntimeRuleResolver {
        old: row.old_row(),
        new: row.new_row(),
        old_doc_id: row.old_doc_id(),
        new_doc_id: row.new_doc_id(),
        columns,
    }
}

struct RuleRowSource {
    clause: FromClause,
    metadata: RuleRowMetadata,
}

struct RuleRowMetadata {
    relation: InternalRelationId,
    old_columns: BTreeMap<String, InternalColumnRef>,
    new_columns: BTreeMap<String, InternalColumnRef>,
    old_row: InternalColumnRef,
    new_row: InternalColumnRef,
    source_index: InternalColumnRef,
    types: Vec<Option<ColumnType>>,
    collations: Vec<Option<u32>>,
}

pub struct BoundSetOrientedAction {
    pub statement: Statement,
    pub source_index: Expr,
}

pub fn bind_set_oriented_action(
    matching_rows: &[usize],
    rows: &[impl RuleRowValues],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    bind_action: &BindRuleAction<'_>,
) -> Result<BoundSetOrientedAction, SQLError> {
    let source = rule_row_source(matching_rows, rows, columns)?;
    bind_source_action(source, bind_action)
}

fn bind_source_action(
    source: RuleRowSource,
    bind_action: &BindRuleAction<'_>,
) -> Result<BoundSetOrientedAction, SQLError> {
    let source_index = Expr::InternalColumn(source.metadata.source_index);
    let mut bound = bind_action(&mut RuleSourceResolver {
        old_columns: &source.metadata.old_columns,
        new_columns: &source.metadata.new_columns,
        old_row: source.metadata.old_row,
        new_row: source.metadata.new_row,
    })?;
    attach_rule_row_source(&mut bound, source.clause, source.metadata.relation)?;
    Ok(BoundSetOrientedAction {
        statement: bound,
        source_index,
    })
}

fn rule_row_source(
    matching_rows: &[usize],
    rows: &[impl RuleRowValues],
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<RuleRowSource, SQLError> {
    let metadata = rule_row_metadata(InternalRelationId::allocate(), columns);
    let values = matching_rows
        .iter()
        .map(|row_index| {
            let row = rows.get(*row_index).ok_or_else(|| {
                SQLError::Internal("rewrite rule lost its qualified row image".into())
            })?;
            let resolver = runtime_rule_resolver(row, columns);
            let mut values = Vec::with_capacity(metadata.types.len());
            for column in columns.keys() {
                values.push(resolver.field_expression(row.old_row(), row.old_doc_id(), column)?);
                values.push(resolver.field_expression(row.new_row(), row.new_doc_id(), column)?);
            }
            values.push(ResolvedVariable::into_expression(
                resolver.record(row.old_row(), row.old_doc_id())?,
            ));
            values.push(ResolvedVariable::into_expression(
                resolver.record(row.new_row(), row.new_doc_id())?,
            ));
            let row_index = i64::try_from(*row_index).map_err(|_| {
                SQLError::Internal("rewrite rule event row index exceeds BIGINT".into())
            })?;
            values.push(Expr::Literal(Value::Int(row_index)));
            Ok(values)
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(RuleRowSource {
        clause: FromClause::Values {
            rows: values,
            alias: None,
            column_aliases: Vec::new(),
            internal_relation: Some(metadata.relation),
            internal_column_types: metadata.types.clone(),
            internal_column_collations: metadata.collations.clone(),
        },
        metadata,
    })
}

fn rule_row_metadata(
    relation: InternalRelationId,
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> RuleRowMetadata {
    let mut internal_column_types = Vec::with_capacity(columns.len() * 2 + 3);
    let mut internal_column_collations = Vec::with_capacity(columns.len() * 2 + 3);
    let mut old_columns = BTreeMap::new();
    let mut new_columns = BTreeMap::new();
    for (index, (column, metadata)) in columns.iter().enumerate() {
        let old = relation.column(index * 2);
        let new = relation.column(index * 2 + 1);
        old_columns.insert(column.clone(), old);
        new_columns.insert(column.clone(), new);
        internal_column_types.push(Some(metadata.ty.clone()));
        internal_column_types.push(Some(metadata.ty.clone()));
        internal_column_collations.extend([metadata.collation_oid; 2]);
    }
    let old_row = relation.column(internal_column_types.len());
    internal_column_types.push(Some(ColumnType::Record));
    let new_row = relation.column(internal_column_types.len());
    internal_column_types.push(Some(ColumnType::Record));
    let source_index = relation.column(internal_column_types.len());
    internal_column_types.push(Some(ColumnType::BigInteger));
    internal_column_collations.extend([None; 3]);
    RuleRowMetadata {
        relation,
        old_columns,
        new_columns,
        old_row,
        new_row,
        source_index,
        types: internal_column_types,
        collations: internal_column_collations,
    }
}

struct RuleSourceResolver<'a> {
    old_columns: &'a BTreeMap<String, InternalColumnRef>,
    new_columns: &'a BTreeMap<String, InternalColumnRef>,
    old_row: InternalColumnRef,
    new_row: InternalColumnRef,
}

impl VariableResolver for RuleSourceResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        Ok(if name.eq_ignore_ascii_case("old") {
            Some(Expr::InternalColumn(self.old_row))
        } else if name.eq_ignore_ascii_case("new") {
            Some(Expr::InternalColumn(self.new_row))
        } else {
            None
        })
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        let columns = if qualifier.eq_ignore_ascii_case("old") {
            self.old_columns
        } else if qualifier.eq_ignore_ascii_case("new") {
            self.new_columns
        } else {
            return Ok(None);
        };
        let source_column = columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))?;
        Ok(Some(Expr::InternalColumn(*source_column)))
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.rewrite_name(qualifier)
    }
}

fn attach_rule_row_source(
    statement: &mut Statement,
    source: FromClause,
    relation: InternalRelationId,
) -> Result<(), SQLError> {
    match statement {
        Statement::Select(select) => attach_select_rule_source(select, &source, relation),
        Statement::Insert(insert) => {
            let select = insert.select_source.as_mut().ok_or_else(|| {
                SQLError::Internal("set-oriented rule INSERT action has no SELECT source".into())
            })?;
            attach_select_rule_source(select, &source, relation);
        }
        Statement::Update(update) => {
            update.from = Some(prepend_rule_row_source(
                update.from.take(),
                source,
                relation,
            ));
        }
        Statement::Delete(delete) => {
            delete.using = Some(prepend_rule_row_source(
                delete.using.take(),
                source,
                relation,
            ));
        }
        _ => {
            return Err(SQLError::Internal(
                "validated rewrite-rule action changed statement kind".into(),
            ))
        }
    }
    Ok(())
}

fn attach_select_rule_source(
    select: &mut SelectStmt,
    source: &FromClause,
    relation: InternalRelationId,
) {
    if let Some(set_op) = select.set_op.as_mut() {
        if let Some(left) = set_op.left.as_mut() {
            attach_select_rule_source(left, source, relation);
        } else {
            select.from = Some(prepend_rule_row_source(
                select.from.take(),
                source.clone(),
                relation,
            ));
        }
        attach_select_rule_source(&mut set_op.right, source, relation);
    } else {
        select.from = Some(prepend_rule_row_source(
            select.from.take(),
            source.clone(),
            relation,
        ));
    }
}

fn prepend_rule_row_source(
    existing: Option<FromClause>,
    source: FromClause,
    relation: InternalRelationId,
) -> FromClause {
    let Some(existing) = existing else {
        return source;
    };
    let lateral = from_references_internal_relation(&existing, relation);
    FromClause::Join {
        left: Box::new(source),
        right: Box::new(existing),
        kind: JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral,
    }
}

struct InternalReferenceResolver {
    relation: InternalRelationId,
    referenced: bool,
}

impl VariableResolver for InternalReferenceResolver {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_internal(&mut self, column: InternalColumnRef) -> Result<Option<Expr>, SQLError> {
        if column.relation() == self.relation {
            self.referenced = true;
        }
        Ok(None)
    }
}

fn expr_references_internal_relation(expr: &Expr, relation: InternalRelationId) -> bool {
    let mut resolver = InternalReferenceResolver {
        relation,
        referenced: false,
    };
    let _ = bind_expr(expr, &mut resolver);
    resolver.referenced
}

fn select_references_internal_relation(select: &SelectStmt, relation: InternalRelationId) -> bool {
    let mut resolver = InternalReferenceResolver {
        relation,
        referenced: false,
    };
    let _ = bind_select(select, &mut resolver);
    resolver.referenced
}

fn from_references_internal_relation(from: &FromClause, relation: InternalRelationId) -> bool {
    match from {
        crate::ast::FromClause::RetainedRows(_) => false,
        FromClause::Table { .. } => false,
        FromClause::Join {
            left, right, on, ..
        } => {
            from_references_internal_relation(left, relation)
                || from_references_internal_relation(right, relation)
                || on
                    .as_ref()
                    .is_some_and(|expr| expr_references_internal_relation(expr, relation))
        }
        FromClause::Values { rows, .. } => rows
            .iter()
            .flatten()
            .any(|expr| expr_references_internal_relation(expr, relation)),
        FromClause::Function { args, .. } => args
            .iter()
            .any(|expr| expr_references_internal_relation(expr, relation)),
        FromClause::FunctionGroup { functions, .. } => functions.iter().any(|function| {
            function
                .args
                .iter()
                .any(|expr| expr_references_internal_relation(expr, relation))
        }),
        FromClause::Subquery { body, .. } => select_references_internal_relation(body, relation),
    }
}
