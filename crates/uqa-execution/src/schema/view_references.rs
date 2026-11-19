//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist relation and column reference rewrites before publishing view registries.
use super::{
    publication::dependencies::CatalogPublicationChanges,
    view_dependencies::{self, ViewDependencyContext},
};
use crate::catalog::{
    context::CatalogContext,
    projection,
    view::{catalog_view_row, ViewPublication, ViewRegistryState},
};
use uqa_core::RelationIdentity;
use uqa_sql::binding::view_dependencies::query_plan_references_relation;
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub struct ViewReferenceContext<'a> {
    pub registry: &'a dyn ViewRegistryState,
    pub publication: &'a dyn ViewPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub dependencies: ViewDependencyContext<'a>,
    pub catalog: CatalogContext<'a>,
}
pub fn rewrite_view_relation_references(
    context: &ViewReferenceContext<'_>,
    replacements: &std::collections::BTreeMap<RelationIdentity, RelationIdentity>,
) -> StorageBackendResult<()> {
    if replacements.is_empty() {
        return Ok(());
    }
    let original = (**context.registry.views_read()).clone();
    let updates = uqa_sql::catalog::stored_view::references::rewritten_relation_references(
        &original,
        replacements,
    )
    .map_err(StorageBackendError::Other)?;
    let mut next = original;
    let mut changed = Vec::with_capacity(updates.len());
    for (relation, view) in updates {
        next.insert(relation.clone(), view).map_err(|error| {
            StorageBackendError::backend("rewrite view relation references", error)
        })?;
        changed.push(relation);
    }
    if context.publication.has_catalog() {
        for relation in &changed {
            let view = &next[relation];
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                context
                    .publication
                    .save_view(&catalog_view_row(relation, view)?)?;
            }
        }
    }
    for relation in changed {
        let view = &next[&relation];
        context.changes.prepared_catalog_changed(
            crate::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                view.relation_oids().relation,
            ),
        );
    }
    **context.registry.views_write() = next;
    Ok(())
}
pub fn views_depending_on_column(
    context: &ViewReferenceContext<'_>,
    table: &str,
    column: &str,
) -> StorageBackendResult<Vec<String>> {
    let candidates = view_dependencies::views_depending_on_relation(&context.dependencies, table)?;
    let views = (**context.registry.views_read()).clone();
    let mut dependent = Vec::new();
    for name in candidates {
        let identity =
            RelationIdentity::from_legacy_name(&name).map_err(StorageBackendError::Other)?;
        if let Some(view) = views.get(&identity) {
            if projection::view_query_references_column(
                &context.catalog,
                &view.query,
                table,
                column,
            )
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
            {
                dependent.push(name);
            }
        }
    }
    Ok(dependent)
}

pub fn rewrite_view_column_references(
    context: &ViewReferenceContext<'_>,
    table: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    let target = RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
    let original = (**context.registry.views_read()).clone();
    let mut next = original.clone();
    let mut changed = Vec::new();
    for (relation, view) in &original {
        if query_plan_references_relation(&view.query, &target, &std::collections::BTreeSet::new())
        {
            let mut view = view.clone();
            projection::rename_view_column_query(
                &context.catalog,
                &mut view.query,
                table,
                from,
                to,
            )
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            next.insert(relation.clone(), view).map_err(|error| {
                StorageBackendError::backend("rewrite view column references", error)
            })?;
            changed.push(relation.clone());
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    if context.publication.has_catalog() {
        for relation in &changed {
            let view = next
                .get(relation)
                .expect("rewritten view retained in replacement catalog");
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                context
                    .publication
                    .save_view(&catalog_view_row(relation, view)?)?;
            }
        }
    }
    for relation in changed {
        if let Some(view) = next.get(&relation) {
            context.changes.prepared_catalog_changed(
                crate::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                    view.relation_oids().relation,
                ),
            );
        }
    }
    **context.registry.views_write() = next;
    context.changes.catalog_registry_changed();
    Ok(())
}
