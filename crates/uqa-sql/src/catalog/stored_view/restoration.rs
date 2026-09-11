//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode and validate durable view definitions and migrated public metadata.
use super::StoredView;
use crate::{catalog::roles::guards::RoleCatalogGuards, plan::QueryPlan, RowSchema};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum RestoredView {
    Current(StoredView),
    Legacy(QueryPlan),
}

pub fn validate_restored_view_object_ids(
    views: &BTreeMap<RelationIdentity, StoredView>,
) -> Result<(), String> {
    let mut object_ids = BTreeSet::new();
    for (relation, view) in views {
        if !object_ids.insert(view.object_id) {
            return Err(format!(
                "view `{}` has a duplicate object identity",
                relation.qualified_name()
            ));
        }
    }
    Ok(())
}

pub fn validate_restored_view_security(
    roles: &dyn RoleCatalogGuards,
    view_name: &str,
    view: &StoredView,
) -> Result<(), String> {
    if !roles.role_definitions().contains_key(&view.role_owner) {
        return Err(format!(
            "view `{view_name}` is owned by missing role `{}`",
            view.role_owner
        ));
    }
    crate::catalog::security::table::validate_table_security_invariants(
        &view.security(),
        view.output_columns.as_deref(),
        &roles.role_definitions(),
    )
    .map_err(|error| format!("view `{view_name}` has invalid privilege metadata: {error}"))?;
    Ok(())
}
pub fn validate_migrated_view_security(
    roles: &dyn RoleCatalogGuards,
    views: &BTreeMap<RelationIdentity, StoredView>,
) -> Result<(), String> {
    for (relation, view) in views {
        let output_columns = view.output_columns.as_deref().ok_or_else(|| {
            format!(
                "view `{}` has no public column metadata after catalog migration",
                relation.qualified_name()
            )
        })?;
        crate::catalog::security::table::validate_table_security_invariants(
            &view.security(),
            Some(output_columns),
            &roles.role_definitions(),
        )
        .map_err(|error| {
            format!(
                "view `{}` has invalid privilege metadata after migration: {error}",
                relation.qualified_name()
            )
        })?;
    }
    Ok(())
}
pub fn restored_view_output_columns(schema: &RowSchema) -> Vec<String> {
    schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect()
}

#[cfg(test)]
mod tests;
