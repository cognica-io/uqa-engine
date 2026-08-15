//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column bindings for allocation-free projected group lookup.

use std::hash::BuildHasher;

use uqa_core::Value;
use uqa_execution::{hash_canonical_row, ExecResult, RowSchema, ScalarExpr};
use uqa_sql::expr::RowLookup;

pub(super) enum ProjectedGroupColumn {
    Position(usize),
}

impl ProjectedGroupColumn {
    pub(super) fn compile(
        expressions: &[ScalarExpr],
        input_schema: &RowSchema,
    ) -> Option<Vec<Self>> {
        expressions
            .iter()
            .map(|expression| {
                super::projected_input::column_slot(expression, input_schema).map(Self::Position)
            })
            .collect()
    }

    #[inline]
    pub(super) fn value<'row, Row: RowLookup>(&self, row: &'row Row) -> Option<&'row Value> {
        match self {
            Self::Position(index) => row.positional_column(*index),
        }
    }
}

pub(super) fn group_hash<S: BuildHasher, Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    row: &Row,
    build_hasher: &S,
) -> ExecResult<u64> {
    hash_canonical_row(build_hasher, columns.iter().map(|column| column.value(row)))
}

#[inline]
pub(super) fn group_matches<Row: RowLookup>(
    columns: &[ProjectedGroupColumn],
    key: &[Value],
    row: &Row,
) -> bool {
    let null = Value::Null;
    key.len() == columns.len()
        && key
            .iter()
            .zip(columns)
            .all(|(stored, column)| stored == column.value(row).unwrap_or(&null))
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
