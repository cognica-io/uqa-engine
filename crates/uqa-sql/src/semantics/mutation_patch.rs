//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint dependency analysis for UPDATE's storage patch path.
use crate::{
    catalog::errors::dml_storage_error,
    semantics::{
        constraint_catalog::ConstraintCatalog,
        referential::{referrers_to_for_actions, ReferentialCatalog},
    },
    SQLError,
};
use std::collections::BTreeSet;
pub fn can_patch_update_without_full_row<'a>(
    catalog: &dyn ConstraintCatalog,
    table: &str,
    referrers: &dyn ReferentialCatalog,
    update_columns: impl IntoIterator<Item = &'a str>,
) -> Result<bool, SQLError> {
    if catalog
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.enforced)
    {
        return Ok(false);
    }
    let update_keys: BTreeSet<&str> = update_columns.into_iter().collect();
    if catalog
        .enforced_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| {
            constraint.predicate.as_deref().is_some_and(|predicate| {
                update_keys.iter().any(|column| {
                    crate::schema::dependencies::schema_expr_references_column(predicate, column)
                })
            }) || constraint.keys.iter().any(|key| {
                update_keys.iter().any(|column| {
                    crate::schema::dependencies::schema_expr_references_column(
                        &key.expression(),
                        column,
                    )
                })
            })
        })
    {
        return Ok(false);
    }
    if catalog
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .filter(|fk| fk.enforced)
        .any(|fk| {
            fk.local_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if referrers_to_for_actions(referrers, table)?
        .iter()
        .any(|(_, fk)| {
            fk.ref_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    Ok(true)
}

pub fn point_lookup_field_is_unique(
    catalog: &dyn ConstraintCatalog,
    table: &str,
    lookup_field: &str,
) -> Result<bool, SQLError> {
    Ok(catalog
        .enforced_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| {
            constraint.predicate.is_none()
                && constraint.keys.len() == 1
                && constraint.keys[0].column() == Some(lookup_field)
        }))
}
