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
pub type KeyConstraintsRead<'a> = Box<dyn Deref<Target = Vec<TableKeyConstraint>> + 'a>;
pub trait ColumnDependencyState {
    fn columns(&self) -> ColumnsRead<'_>;
    fn foreign_keys(&self) -> ForeignKeysRead<'_>;
    fn key_constraints(&self) -> KeyConstraintsRead<'_>;
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

    // Dropping a column that a key constraint includes drops the constraint, so a foreign key that references the constraint's key depends on the column too.
    let included_keys = target_state
        .key_constraints()
        .iter()
        .filter(|constraint| {
            constraint
                .included_columns
                .iter()
                .any(|name| name == column)
        })
        .map(|constraint| (constraint.kind, constraint.columns.clone()))
        .collect::<Vec<_>>();
    let references_included_key = |referenced: Option<&[String]>| {
        included_keys.iter().any(|(kind, key)| match referenced {
            Some(referenced) => key.as_slice() == referenced,
            // A reference without columns is to the primary key.
            None => *kind == crate::ast::TableKeyConstraintKind::PrimaryKey,
        })
    };
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
                    && (foreign_key.ref_columns.iter().any(|name| name == column)
                        || references_included_key(
                            (!foreign_key.ref_columns.is_empty())
                                .then_some(foreign_key.ref_columns.as_slice()),
                        ));
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
                    && (reference.column.as_deref() == Some(column)
                        || references_included_key(
                            reference.column.as_ref().map(std::slice::from_ref),
                        ))
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
            definition.check_catalog_oid = None;
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
/// A key constraint goes with any column of its supporting index: a key column or an included one. A single-column constraint that goes with an included column also gives up the key flag of its surviving key column, unless another constraint of its kind still keys that column alone.
pub fn remove_column_keys(
    columns: &mut [ColumnDef],
    keys: &mut Vec<TableKeyConstraint>,
    column: &str,
) {
    let mut removed = Vec::new();
    keys.retain(|constraint| {
        let goes = constraint
            .columns
            .iter()
            .chain(&constraint.included_columns)
            .any(|name| name == column);
        if goes {
            removed.push(constraint.clone());
        }
        !goes
    });
    for constraint in removed {
        let [key] = constraint.columns.as_slice() else {
            continue;
        };
        if keys
            .iter()
            .any(|kept| kept.kind == constraint.kind && kept.columns.as_slice() == [key.as_str()])
        {
            continue;
        }
        if let Some(definition) = columns
            .iter_mut()
            .find(|definition| definition.name == *key)
        {
            match constraint.kind {
                crate::ast::TableKeyConstraintKind::PrimaryKey => definition.primary_key = false,
                crate::ast::TableKeyConstraintKind::Unique => definition.unique = false,
            }
        }
    }
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
