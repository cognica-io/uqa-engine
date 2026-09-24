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
    Current(super::StoredViewDefinition),
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
    view.security
        .validate(view.output_columns.as_deref(), &roles.role_definitions())
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
        view.security
            .validate(Some(output_columns), &roles.role_definitions())
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
