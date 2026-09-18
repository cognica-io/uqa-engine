//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore view catalogs and migrate legacy definitions within the caller's catalog transaction.
pub mod context;
mod migration;
use super::{catalog_view_row, StoredView, StoredViewKind};
use context::{ViewRestoreContext, ViewRowsStorage};
use migration::migrate_persisted_views;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::{
    binding::view_dependencies::{
        bind_query_plan_sequence_references, query_plan_has_legacy_routine_identity,
        restoration::{bind_stored_view_relations, upgrade_legacy_view_dispatches},
    },
    catalog::stored_view::restoration::{self as analysis, RestoredView},
};
use uqa_storage::{StorageBackendError, StorageBackendResult, ViewRow};

fn temporary_stored_views(
    context: &ViewRestoreContext<'_>,
) -> BTreeMap<RelationIdentity, StoredView> {
    context
        .registry
        .views_read()
        .iter()
        .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
        .map(|(relation, view)| (relation.clone(), view.clone()))
        .collect()
}
fn restored_view_relation_namespace(
    context: &ViewRestoreContext<'_>,
    rows: &[ViewRow],
    temporary_views: &BTreeMap<RelationIdentity, StoredView>,
) -> StorageBackendResult<BTreeSet<RelationIdentity>> {
    let mut relations = context
        .namespace
        .tables()
        .names()
        .cloned()
        .collect::<BTreeSet<_>>();
    relations.extend(context.namespace.foreign_tables().keys().cloned());
    relations.extend(rows.iter().map(|row| row.relation.clone()));
    for (graph, store) in context.namespace.graphs().iter() {
        let labels = store.graph_labels(graph).map_err(|error| {
            StorageBackendError::Other(format!(
                "restore graph-label view dependencies for `{graph}`: {error}"
            ))
        })?;
        relations.extend(
            labels
                .into_iter()
                .map(|label| RelationIdentity::new(graph, label.name)),
        );
    }
    relations.extend(temporary_views.keys().cloned());
    Ok(relations)
}
fn validate_and_persist_restored_views(
    context: &ViewRestoreContext<'_>,
    catalog: &dyn ViewRowsStorage,
    views: &BTreeMap<RelationIdentity, StoredView>,
    migrated_views: &BTreeSet<RelationIdentity>,
    revised_rows: &BTreeSet<RelationIdentity>,
) -> StorageBackendResult<()> {
    analysis::validate_migrated_view_security(context.roles, views)
        .map_err(StorageBackendError::Other)?;
    for relation in revised_rows {
        if migrated_views.contains(relation) {
            continue;
        }
        let view = views.get(relation).ok_or_else(|| {
            StorageBackendError::Other(format!(
                "updated view `{}` disappeared during restoration",
                relation.qualified_name()
            ))
        })?;
        catalog.save_view(&catalog_view_row(relation, view)?)?;
    }
    Ok(())
}
fn restore_view(
    context: &ViewRestoreContext<'_>,
    row: &ViewRow,
    allows_migration: bool,
) -> StorageBackendResult<(StoredView, bool)> {
    let view_name = row.relation.qualified_name();
    let (definition, legacy_query) =
        match serde_json::from_str::<RestoredView>(&row.definition_json)? {
            RestoredView::Current(view) => (view, false),
            RestoredView::Legacy(query) => (
                uqa_sql::catalog::stored_view::StoredViewDefinition {
                    object_id: [0; 16],
                    query,
                    output_columns: None,
                    persistence: uqa_sql::ast::RelationPersistence::Permanent,
                    options: Vec::new(),
                    kind: StoredViewKind::View,
                    materialized_rows: Vec::new(),
                    materialized_column_types: Vec::new(),
                    populated: true,
                },
                true,
            ),
        };
    let security = crate::catalog::security::relation_restoration::restore_security(
        &row.security,
        definition.output_columns.as_deref(),
        &context.roles.role_definitions(),
        allows_migration,
    )
    .map_err(|error| {
        StorageBackendError::Other(format!(
            "view `{view_name}` has invalid security metadata: {error}"
        ))
    })?;
    Ok((
        StoredView {
            security,
            definition,
        },
        legacy_query,
    ))
}

pub fn restore_views_from_catalog(
    context: &ViewRestoreContext<'_>,
    catalog: &dyn ViewRowsStorage,
    allows_migration: bool,
) -> StorageBackendResult<()> {
    let rows = catalog.load_views()?;
    let temporary_views = temporary_stored_views(context);
    let relations = restored_view_relation_namespace(context, &rows, &temporary_views)?;

    let mut views = BTreeMap::new();
    let mut routine_binding_migrations = BTreeSet::new();
    let mut missing_output_columns = Vec::new();
    let mut missing_object_ids = Vec::new();
    let mut revised_rows = BTreeSet::new();
    for row in rows {
        let view_name = row.relation.qualified_name();
        let (mut view, legacy_query) = restore_view(context, &row, allows_migration)?;
        if legacy_query {
            routine_binding_migrations.insert(row.relation.clone());
        }
        if matches!(row.security, uqa_storage::RelationSecurityRow::Legacy(_)) {
            revised_rows.insert(row.relation.clone());
        }
        if view.object_id == [0; 16] {
            view.object_id = context.identities.allocate_identity()?;
            missing_object_ids.push(row.relation.clone());
        }
        if view.kind == StoredViewKind::View && view.output_columns.is_none() {
            missing_output_columns.push(row.relation.clone());
        }
        if upgrade_legacy_view_dispatches(&mut view.query) {
            revised_rows.insert(row.relation.clone());
        }
        if query_plan_has_legacy_routine_identity(&view.query) {
            routine_binding_migrations.insert(row.relation.clone());
        }
        bind_stored_view_relations(&mut view.query, &relations).map_err(|error| {
            StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
        })?;
        bind_query_plan_sequence_references(&mut view.query, &mut |reference| {
            context.sequences.resolve_loaded(reference)
        })
        .map_err(|error| {
            StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
        })?;
        views.insert(row.relation, view);
    }
    views.extend(temporary_views);
    analysis::validate_restored_view_object_ids(&views).map_err(StorageBackendError::Other)?;
    if !allows_migration
        && (!routine_binding_migrations.is_empty()
            || !missing_output_columns.is_empty()
            || !missing_object_ids.is_empty()
            || !revised_rows.is_empty())
    {
        return Err(StorageBackendError::Other(
            "view catalog requires an initial-open metadata migration".into(),
        ));
    }
    migrate_persisted_views(
        context,
        catalog,
        &mut views,
        &routine_binding_migrations,
        &missing_output_columns,
        &missing_object_ids,
    )?;
    let migrated_views = routine_binding_migrations
        .iter()
        .chain(&missing_output_columns)
        .chain(&missing_object_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    validate_and_persist_restored_views(context, catalog, &views, &migrated_views, &revised_rows)?;
    **context.registry.views_write() = views;
    Ok(())
}

#[cfg(test)]
mod tests;
