//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Effective constraint views over stored column and table declarations.
use crate::ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint, TableKeyConstraintKind};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub fn column_checks(columns: &[ColumnDef]) -> Vec<TableCheck> {
    let mut out = Vec::new();
    for column in columns {
        if let Some(expr) = column.check.clone() {
            out.push(TableCheck {
                name: column
                    .check_name
                    .clone()
                    .or_else(|| Some(format!("{}_check", column.name))),
                expr,
                enforced: column.check_enforced,
                validated: column.check_validated,
                no_inherit: column.check_no_inherit,
                object_id: column.check_object_id,
                is_local: column.check_is_local,
                partition_constraint: None,
            });
        }
    }
    out
}
pub fn append_column_foreign_keys(columns: &[ColumnDef], foreign_keys: &mut Vec<ForeignKey>) {
    for column in columns {
        if let Some(reference) = &column.references {
            let mut foreign_key = super::foreign_keys::column_foreign_key(column, reference);
            foreign_key.name = foreign_key
                .name
                .or_else(|| Some(format!("{}_fkey", column.name)));
            foreign_keys.push(foreign_key);
        }
    }
}
pub fn append_column_keys(columns: &[ColumnDef], constraints: &mut Vec<TableKeyConstraint>) {
    for column in columns {
        let kind = if column.primary_key {
            Some(TableKeyConstraintKind::PrimaryKey)
        } else if column.unique {
            Some(TableKeyConstraintKind::Unique)
        } else {
            None
        };
        let Some(kind) = kind else {
            continue;
        };
        if constraints.iter().any(|constraint| {
            constraint.kind == kind
                && constraint.columns.as_slice() == std::slice::from_ref(&column.name)
        }) {
            continue;
        }
        constraints.push(TableKeyConstraint {
            name: None,
            kind,
            columns: vec![column.name.clone()],
            nulls_not_distinct: false,
            without_overlaps: false,
        });
    }
}
pub fn auto_increment_columns(columns: &[ColumnDef]) -> BTreeSet<String> {
    columns
        .iter()
        .filter(|column| column.auto_increment.is_some())
        .map(|column| column.name.clone())
        .collect()
}
pub fn unique_scalar_columns(
    constraints: Vec<TableKeyConstraint>,
    auto_increment: &BTreeSet<String>,
) -> Vec<String> {
    constraints
        .into_iter()
        .filter(|constraint| constraint.columns.len() == 1)
        .map(|constraint| constraint.columns[0].clone())
        .filter(|column| !auto_increment.contains(column))
        .collect()
}
/// Loaded table identities only; persisted references do not consult the session search path.
pub trait StoredTableNames {
    fn stored_table_exists(&self, relation: &RelationIdentity) -> bool;
    fn stored_table_names(&self) -> Vec<RelationIdentity>;
}
pub fn canonical_stored_foreign_key_target(
    catalog: &dyn StoredTableNames,
    reference: &str,
) -> Result<String, String> {
    let (schema, local_name) = RelationIdentity::parse_reference(reference)
        .map_err(|error| format!("invalid persisted foreign-key target `{reference}`: {error}"))?;
    if let Some(schema) = schema {
        let target = RelationIdentity::new(schema, local_name);
        if catalog.stored_table_exists(&target) {
            return Ok(target.qualified_name());
        }
        return Err(format!(
            "dangling persisted foreign-key target `{reference}`"
        ));
    }
    let candidates = catalog
        .stored_table_names()
        .into_iter()
        .filter(|candidate| candidate.name == local_name)
        .map(|candidate| candidate.qualified_name())
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err(format!(
            "dangling persisted foreign-key target `{reference}`"
        )),
        _ => Err(format!(
            "ambiguous persisted foreign-key target `{reference}` matches {}",
            candidates.join(", ")
        )),
    }
}
pub fn bind_stored_foreign_key_targets(
    catalog: &dyn StoredTableNames,
    foreign_keys: &mut [ForeignKey],
) -> Result<(), String> {
    for foreign_key in foreign_keys {
        foreign_key.ref_table =
            canonical_stored_foreign_key_target(catalog, &foreign_key.ref_table)?;
    }
    Ok(())
}
