//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-column dependency validation and pure declaration rewrites.

#[cfg(test)]
mod tests;
use crate::ast::{ColumnDef, TableCheck};
use crate::schema::dependencies::schema_expr_references_column;

pub fn clear_foreign_column_default(columns: &mut [ColumnDef], column_name: &str) -> bool {
    let Some(column) = columns.iter_mut().find(|column| column.name == column_name) else {
        return false;
    };
    column.default.take().is_some()
}

pub fn remove_foreign_check(
    columns: &mut [ColumnDef],
    checks: &mut Vec<TableCheck>,
    constraint_name: &str,
) -> bool {
    for column in columns {
        if column.check.is_some() && column.check_name.as_deref() == Some(constraint_name) {
            column.check = None;
            column.check_name = None;
            column.check_object_id = None;
            column.check_is_local = true;
            column.check_enforced = true;
            column.check_validated = true;
            column.check_no_inherit = false;
            return true;
        }
    }
    let Some(index) = checks
        .iter()
        .position(|check| check.name.as_deref() == Some(constraint_name))
    else {
        return false;
    };
    checks.remove(index);
    true
}

pub fn validate_foreign_column_removal(
    columns: &[ColumnDef],
    table_name: &str,
    column_name: &str,
) -> Result<(), String> {
    for column in columns {
        if column.name == column_name {
            continue;
        }
        if column
            .default
            .as_ref()
            .is_some_and(|expression| schema_expr_references_column(expression, column_name))
            || column.generated.as_ref().is_some_and(|generated| {
                schema_expr_references_column(&generated.expression, column_name)
            })
        {
            return Err(format!(
                    "cannot drop generated column `{table_name}`.`{column_name}` because column `{}` depends on it",
                    column.name
                ));
        }
    }
    Ok(())
}

pub fn remove_foreign_column(
    columns: &mut Vec<ColumnDef>,
    checks: &mut Vec<TableCheck>,
    column_index: usize,
    column_name: &str,
) {
    for column in columns.iter_mut() {
        if column.name != column_name
            && column
                .check
                .as_ref()
                .is_some_and(|expression| schema_expr_references_column(expression, column_name))
        {
            column.check = None;
            column.check_name = None;
            column.check_object_id = None;
            column.check_is_local = true;
            column.check_enforced = true;
            column.check_validated = true;
            column.check_no_inherit = false;
        }
    }
    columns.remove(column_index);
    checks.retain(|check| !schema_expr_references_column(&check.expr, column_name));
}

pub fn detach_foreign_sequence_provenance(columns: &mut [ColumnDef], sequence: &str) -> bool {
    let mut changed = false;
    for column in columns {
        if column
            .auto_increment
            .as_ref()
            .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
        {
            column.auto_increment = None;
            changed = true;
        }
    }
    changed
}
