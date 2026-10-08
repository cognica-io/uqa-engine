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

pub fn rewrite_view_routine_identity(
    context: &ViewDependencyContext<'_>,
    target: &FunctionBinding,
    new_name: &str,
) -> StorageBackendResult<()> {
    let views = context.views.view_definitions();
    let mut next = (**views).clone();
    drop(views);
    let changed = analysis::rewrite_view_routine_identity(&mut next, target, new_name);
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
    let changed = analysis::rewrite_view_type_references(&mut next, oid, identity);
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
    let views = context.views.view_definitions();
    let mut next = (**views).clone();
    drop(views);
    let mut changed = Vec::new();
    for (relation, view) in &mut next {
        let query_changed = constants.query(&mut view.query)?;
        let columns = view
            .output_columns
            .clone()
            .unwrap_or_default()
            .into_iter()
            .zip(view.materialized_column_types.clone())
            .filter_map(|(name, ty)| ty.map(|ty| (name, ty)))
            .collect::<Vec<_>>();
        let mut affected = Vec::new();
        for (name, ty) in columns {
            if uqa_sql::expr::composites::type_contains_composite(&ty, target, types)? {
                affected.push((name, ty));
            }
        }
        if affected.is_empty() && !query_changed {
            continue;
        }
        for row in &mut view.materialized_rows {
            for (name, ty) in &affected {
                if let Some(value) = row.get(name).cloned() {
                    let value = uqa_sql::expr::composites::apply_attribute_change(
                        value, ty, target, change, types,
                    )?;
                    row.insert(name.clone(), value);
                }
            }
        }
        changed.push(relation.clone());
    }
    publish_rewritten_views(context, next, &changed, constants.rename.is_some()).map_err(storage)
}

fn publish_rewritten_views(
    context: &ViewDependencyContext<'_>,
    next: std::collections::BTreeMap<RelationIdentity, uqa_sql::catalog::stored_view::StoredView>,
    changed: &[RelationIdentity],
    expression_names: bool,
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
                if expression_names {
                    context.publication.save_view_expression_names(&row)?;
                } else {
                    context.publication.save_view(&row)?;
                }
            }
        }
    }
    for relation in changed {
        if let Some(view) = next.get(relation).filter(|_| !expression_names) {
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
