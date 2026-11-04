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
        &**context.views.view_definitions(),
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
        &**context.views.view_definitions(),
        &target,
    ))
}

pub fn views_depending_on_function(
    context: &ViewDependencyContext<'_>,
    target: &FunctionBinding,
) -> StorageBackendResult<Vec<String>> {
    context.views.synchronize_catalog()?;
    Ok(analysis::views_depending_on_function(
        &**context.views.view_definitions(),
        target,
    ))
}

pub fn rewrite_view_routine_identity(
    context: &ViewDependencyContext<'_>,
    target: &FunctionBinding,
    new_name: &str,
) -> StorageBackendResult<()> {
    let views = context.views.view_definitions();
    let mut next = (**views).clone();
    drop(views);
    let changed = analysis::rewrite_view_routine_identity(&mut next, target, new_name)
        .map_err(|error| StorageBackendError::backend("rewrite view routine identity", error))?;
    publish_rewritten_views(context, next, &changed, false)
}

/// Views embed the catalog names of the user-defined types they bind; a rename or schema move rewrites them.
pub fn rewrite_view_type_references(
    context: &ViewDependencyContext<'_>,
    oid: u32,
    identity: &RelationIdentity,
) -> StorageBackendResult<()> {
    context.views.synchronize_catalog()?;
    let views = context.views.view_definitions();
    let mut next = (**views).clone();
    drop(views);
    let changed = analysis::rewrite_view_type_references(&mut next, oid, identity)
        .map_err(|error| StorageBackendError::backend("rewrite view type references", error))?;
    publish_rewritten_views(context, next, &changed, false)
}

/// Materialized views store their rows, so a composite type's attribute change rewrites the stored values of the type in each column whose declared type holds it.
pub fn rewrite_materialized_composite_values(
    context: &ViewDependencyContext<'_>,
    target: u32,
    change: &uqa_sql::expr::composites::AttributeChange,
    types: &dyn uqa_sql::expr::composites::CompositeTypeCatalog,
    constants: &uqa_sql::expr::composites::constants::CompositeConstantChange<'_>,
) -> Result<(), uqa_sql::SQLError> {
    let storage = |error: StorageBackendError| {
        uqa_sql::catalog::errors::storage_error("rewrite materialized view rows", &error)
    };
    context.views.synchronize_catalog().map_err(storage)?;
    let original = (**context.views.view_definitions()).clone();
    let mut next = original.clone();
    let mut changed = Vec::new();
    for (relation, view) in &original {
        let query = constants.rewritten_query(&view.query)?;
        let columns = view
            .output_columns
            .iter()
            .flatten()
            .zip(&view.materialized_column_types)
            .filter_map(|(name, ty)| ty.as_ref().map(|ty| (name, ty)));
        let mut affected = Vec::new();
        for (name, ty) in columns {
            if uqa_sql::expr::composites::type_contains_composite(ty, target, types)? {
                affected.push((name, ty));
            }
        }
        if affected.is_empty() && query.is_none() {
            continue;
        }
        let mut view = view.clone();
        if let Some(query) = query {
            view.query = query;
        }
        for row in &mut view.materialized_rows {
            for (name, ty) in &affected {
                if let Some(value) = row.get(*name).cloned() {
                    let value = uqa_sql::expr::composites::apply_attribute_change(
                        value, ty, target, change, types,
                    )?;
                    row.insert((*name).clone(), value);
                }
            }
        }
        next.insert(relation.clone(), view)?;
        changed.push(relation.clone());
    }
    // Retaining the same admitted datum under another descriptor changes its representation, not the view's SQL definition. Existing prepared scalar constants retain their original inputs.
    let same_definition = constants.rename.is_some()
        || matches!(
            change,
            uqa_sql::expr::composites::AttributeChange::Type { .. }
        );
    publish_rewritten_views(context, next, &changed, same_definition).map_err(storage)
}

fn publish_rewritten_views(
    context: &ViewDependencyContext<'_>,
    next: uqa_sql::catalog::stored_view::ViewDefinitions,
    changed: &[RelationIdentity],
    same_definition: bool,
) -> StorageBackendResult<()> {
    if changed.is_empty() {
        return Ok(());
    }
    if context.publication.has_catalog() {
        for relation in changed {
            let view = next.get(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "rewritten view `{}` disappeared before persistence",
                    relation.qualified_name()
                ))
            })?;
            if view.persistence != RelationPersistence::Temporary {
                let row = catalog_view_row(relation, view)?;
                if same_definition {
                    context.publication.save_view_representation(&row)?;
                } else {
                    context.publication.save_view(&row)?;
                }
            }
        }
    }
    for relation in changed {
        if let Some(view) = next.get(relation).filter(|_| !same_definition) {
            context.changes.prepared_catalog_changed(
                crate::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                    view.relation_oids().relation,
                ),
            );
        }
    }
    **context.publication.views_write() = next;
    context.changes.catalog_registry_changed();
    Ok(())
}
