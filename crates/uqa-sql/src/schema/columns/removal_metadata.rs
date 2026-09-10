//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate column dependencies against retained relation metadata and remove local declarations.
use crate::ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint};
use crate::schema::dependencies::{
    rewrites::stored_relation_reference_matches, schema_expr_references_column,
};
use std::ops::Deref;
use uqa_core::RelationIdentity;
pub type ColumnsRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub type ForeignKeysRead<'a> = Box<dyn Deref<Target = Vec<ForeignKey>> + 'a>;
pub trait ColumnDependencyState {
    fn columns(&self) -> ColumnsRead<'_>;
    fn foreign_keys(&self) -> ForeignKeysRead<'_>;
}
pub type ColumnDependencyEntries<'a> = Vec<(String, Box<dyn ColumnDependencyState + 'a>)>;
pub fn validate_column_dependencies(
    target: &RelationIdentity,
    table_name: &str,
    column: &str,
    entries: &[(String, Box<dyn ColumnDependencyState + '_>)],
) -> Result<(), String> {
    let target_state = entries
        .iter()
        .find(|(name, _)| name == table_name)
        .map(|(_, state)| state)
        .ok_or_else(|| format!("table `{table_name}` does not exist"))?;
    for candidate in target_state.columns().iter() {
        if candidate.name == column {
            continue;
        }
        if candidate
            .default
            .as_ref()
            .is_some_and(|expr| schema_expr_references_column(expr, column))
            || candidate.generated.as_ref().is_some_and(|generated| {
                schema_expr_references_column(&generated.expression, column)
            })
        {
            return Err(format!(
                    "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: column `{}` has a dependent DEFAULT/generation expression",
                    candidate.name
                ));
        }
    }

    let mut inbound = Vec::new();
    for (candidate_name, table) in entries {
        for foreign_key in table.foreign_keys().iter() {
            let local_dependency = candidate_name == table_name
                && (foreign_key.local_columns.iter().any(|name| name == column)
                    || foreign_key
                        .on_delete_set_columns
                        .iter()
                        .any(|name| name == column));
            let referenced_dependency =
                stored_relation_reference_matches(&foreign_key.ref_table, target)
                    && foreign_key.ref_columns.iter().any(|name| name == column);
            if referenced_dependency && !local_dependency {
                inbound.push(candidate_name.clone());
            }
        }
        for candidate in table.columns().iter() {
            if candidate_name == table_name && candidate.name == column {
                continue;
            }
            if candidate.references.as_ref().is_some_and(|reference| {
                stored_relation_reference_matches(&reference.table, target)
                    && reference.column.as_deref() == Some(column)
            }) {
                inbound.push(candidate_name.clone());
            }
        }
    }
    inbound.sort_unstable();
    inbound.dedup();
    if !inbound.is_empty() {
        return Err(format!(
                "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: referenced by foreign key(s) on `{}`",
                inbound.join("`, `")
            ));
    }
    Ok(())
}
pub fn remove_column_declarations(columns: &mut Vec<ColumnDef>, column: &str) {
    for definition in columns.iter_mut() {
        if definition
            .check
            .as_ref()
            .is_some_and(|expression| schema_expr_references_column(expression, column))
        {
            definition.check = None;
            definition.check_name = None;
            definition.check_object_id = None;
            definition.check_is_local = true;
            definition.check_enforced = true;
            definition.check_validated = true;
            definition.check_no_inherit = false;
        }
    }
    columns.retain(|c| c.name != column);
}
pub fn remove_column_checks(checks: &mut Vec<TableCheck>, column: &str) {
    checks.retain(|constraint| !schema_expr_references_column(&constraint.expr, column));
}
pub fn remove_column_keys(keys: &mut Vec<TableKeyConstraint>, column: &str) {
    keys.retain(|constraint| !constraint.columns.iter().any(|name| name == column));
}
pub fn remove_column_foreign_keys(keys: &mut Vec<ForeignKey>, column: &str) {
    keys.retain(|foreign_key| {
        !foreign_key.local_columns.iter().any(|name| name == column)
            && !foreign_key
                .on_delete_set_columns
                .iter()
                .any(|name| name == column)
    });
}
