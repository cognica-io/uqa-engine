//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Traverse synchronized view catalogs and persist routine renames before replacing the live registry.

use super::{
    publication::dependencies::CatalogPublicationChanges,
    sequences::dependencies::ViewCatalogPublication, view_alteration::ViewAlterPublication,
};
use crate::catalog::view::catalog_view_row;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{FunctionBinding, RelationPersistence},
    catalog::stored_view::dependencies as analysis,
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub struct ViewDependencyContext<'a> {
    pub views: &'a dyn ViewCatalogPublication,
    pub publication: &'a dyn ViewAlterPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}

pub fn views_depending_on_relation(
    context: &ViewDependencyContext<'_>,
    canonical_name: &str,
) -> StorageBackendResult<Vec<String>> {
    context.views.synchronize_catalog()?;
    let target =
        RelationIdentity::from_legacy_name(canonical_name).map_err(StorageBackendError::Other)?;
    Ok(analysis::views_depending_on_relation(
        &context.views.view_definitions(),
        &target,
    ))
}

pub fn views_depending_on_sequence(
    context: &ViewDependencyContext<'_>,
    canonical_name: &str,
) -> StorageBackendResult<Vec<String>> {
    context.views.synchronize_catalog()?;
    let target =
        RelationIdentity::from_legacy_name(canonical_name).map_err(StorageBackendError::Other)?;
    Ok(analysis::views_depending_on_sequence(
        &context.views.view_definitions(),
        &target,
    ))
}

pub fn views_depending_on_function(
    context: &ViewDependencyContext<'_>,
    target: &FunctionBinding,
) -> StorageBackendResult<Vec<String>> {
    context.views.synchronize_catalog()?;
    Ok(analysis::views_depending_on_function(
        &context.views.view_definitions(),
        target,
    ))
}

pub fn cascade_view_closure(
    context: &ViewDependencyContext<'_>,
    initial: Vec<String>,
) -> Result<Vec<String>, SQLError> {
    let mut views = initial;
    views.sort();
    views.dedup();
    let mut index = 0;
    while index < views.len() {
        let dependents = views_depending_on_relation(context, &views[index]).map_err(|error| {
            SQLError::Internal(format!("read cascading view dependencies: {error}"))
        })?;
        for dependent in dependents {
            if !views.contains(&dependent) {
                views.push(dependent);
            }
        }
        index += 1;
    }
    views.sort();
    Ok(views)
}

pub fn rewrite_view_routine_identity(
    context: &ViewDependencyContext<'_>,
    target: &FunctionBinding,
    new_name: &str,
) -> StorageBackendResult<()> {
    let views = context.views.view_definitions();
    let mut next = (**views).clone();
    drop(views);
    let changed = analysis::rewrite_view_routine_identity(&mut next, target, new_name);
    if changed.is_empty() {
        return Ok(());
    }
    if context.publication.has_catalog() {
        for relation in &changed {
            let view = next.get(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "rewritten view `{}` disappeared before persistence",
                    relation.qualified_name()
                ))
            })?;
            if view.persistence != RelationPersistence::Temporary {
                context
                    .publication
                    .save_view(&catalog_view_row(relation, view)?)?;
            }
        }
    }
    **context.publication.views_write() = next;
    context.changes.catalog_registry_changed();
    Ok(())
}
