//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize the grouping columns used by aggregate evaluation.

use crate::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::plan::QueryBlockPlan;
use uqa_sql::ScalarExpr;

pub use uqa_sql::semantics::aggregates::*;

pub fn group_context_row(stmt: &QueryBlockPlan, group_values: &[Value]) -> OwnedPhysicalRow {
    let mut columns = Vec::new();
    let mut identities = Vec::new();
    let mut values = Vec::new();
    for (expr, value) in stmt.group_by.iter().zip(group_values) {
        match expr {
            ScalarExpr::Column(column) => {
                columns.push(column.clone());
                identities.push(ColumnIdentity::unqualified(column.clone()));
                values.push(value.clone());
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                columns.push(column.clone());
                identities.push(ColumnIdentity::qualified(qualifier.clone(), column.clone()));
                values.push(value.clone());
            }
            _ => {}
        }
    }
    let schema = RowSchema::with_identities(columns, identities, vec![None; values.len()]);
    OwnedPhysicalRow::new(schema, PhysicalRow::from_values(values))
}
