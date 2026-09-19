//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate inherited constraints on descendants before publishing parent validation.

use super::{table_constraint_state, validate_and_mark_constraint, ConstraintAlterContext};
use uqa_sql::{
    ast::TableLockMode,
    schema::constraint_changes::validation::{constraint_validation, ensure_validation_recurses},
    SQLError,
};

pub fn validate_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    recurse: bool,
) -> Result<(), SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    let target = constraint_validation(table, name, &columns, &constraints)?;
    if target.validated {
        return Ok(());
    }
    if target.requires_descendants() {
        let targets = context.rows.catalog.hierarchy_scan_tables(table, true)?;
        ensure_validation_recurses(recurse, targets.iter().any(|target| target != table))?;
        for child in targets.iter().filter(|target| target.as_str() != table) {
            context
                .locks
                .lock_relation(child, TableLockMode::ShareUpdateExclusive)?;
            let (child_columns, child_constraints) = table_constraint_state(context, child)?;
            let child_name = target.child_constraint_name(name, child, &child_columns)?;
            let child_target =
                constraint_validation(child, child_name, &child_columns, &child_constraints)?;
            if !child_target.validated {
                validate_and_mark_constraint(context, child, child_name)?;
            }
        }
    }
    validate_and_mark_constraint(context, table, name)
}
