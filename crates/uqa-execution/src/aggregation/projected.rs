//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column bindings for allocation-free projected group lookup.

use std::hash::BuildHasher;

use crate::{try_pack_compact_text_pair, ExecResult, RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::expr::RowLookup;

pub(super) enum ProjectedGroupColumn {
    Position(usize, Option<uqa_sql::ColumnType>),
    Text(usize),
}

impl ProjectedGroupColumn {
    pub(super) fn compile(
        expressions: &[ScalarExpr],
        input_schema: &RowSchema,
    ) -> Option<Vec<Self>> {
        expressions
            .iter()
            .map(|expression| {
                super::projected_input::column_slot(expression, input_schema).map(|position| {
                    if matches!(
                        input_schema.column_type(position),
                        Some(uqa_sql::ast::ColumnType::Text)
                    ) {
                        Self::Text(position)
                    } else {
                        Self::Position(position, input_schema.column_type(position).cloned())
                    }
                })
            })
            .collect()
    }

    fn ty(&self) -> Option<&uqa_sql::ColumnType> {
        match self {
            Self::Position(_, ty) => ty.as_ref(),
            Self::Text(_) => None,
        }
    }

    #[inline]
    pub(super) fn value<'row, Row: RowLookup>(&self, row: &'row Row) -> Option<&'row Value> {
        match self {
            Self::Position(index, _) | Self::Text(index) => row.positional_column(*index),
        }
    }
}

pub(super) fn group_hash<S: BuildHasher, Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    row: &Row,
    build_hasher: &S,
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> ExecResult<u64> {
    if let Some(key) = compact_text_pair(columns, row) {
        return Ok(key);
    }
    crate::distinct::hash_typed_row_with_catalog(
        build_hasher,
        columns
            .iter()
            .map(|column| (column.value(row), column.ty())),
        enums,
    )
}

pub(super) fn compact_text_pair<Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    row: &Row,
) -> Option<u64> {
    let [ProjectedGroupColumn::Text(first), ProjectedGroupColumn::Text(second)] = columns else {
        return None;
    };
    try_pack_compact_text_pair(
        [
            row.positional_column(*first),
            row.positional_column(*second),
        ]
        .into_iter(),
    )
}

#[inline]
pub(super) fn group_matches<Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    key: &[Value],
    row: &Row,
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> Result<bool, uqa_sql::SQLError> {
    let null = Value::Null;
    if key.len() != columns.len() {
        return Ok(false);
    }
    for (stored, column) in key.iter().zip(columns) {
        let value = column.value(row);
        let equal = match (column, stored, value) {
            (ProjectedGroupColumn::Text(_), Value::Str(stored), Some(Value::Str(value))) => {
                stored == value
            }
            (ProjectedGroupColumn::Text(_), Value::Null, None | Some(Value::Null)) => true,
            _ => uqa_sql::expr::equal_selected_values_with_catalog(
                stored,
                value.unwrap_or(&null),
                column.ty(),
                &uqa_core::memory::ProductionControl::uncontrolled(),
                enums,
            )?,
        };
        if !equal {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn group_key<Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    row: &Row,
    null: &Value,
) -> Vec<Value> {
    columns
        .iter()
        .map(|column| column.value(row).unwrap_or(null).clone())
        .collect()
}
