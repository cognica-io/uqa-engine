//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provisional view registry publication and rollback during metadata migration.
use super::{
    catalog_view_row,
    context::{ViewRestoreContext, ViewRowsStorage},
    StoredView,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::semantics::sets::placement_scope::SetPlacementScope;
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) fn migrate_persisted_views(
    context: &ViewRestoreContext<'_>,
    catalog: &dyn ViewRowsStorage,
    views: &mut BTreeMap<RelationIdentity, StoredView>,
    routine_binding_migrations: &BTreeSet<RelationIdentity>,
    missing_output_columns: &[RelationIdentity],
    missing_object_ids: &[RelationIdentity],
    missing_collations: &BTreeSet<RelationIdentity>,
) -> StorageBackendResult<()> {
    if routine_binding_migrations.is_empty()
        && missing_output_columns.is_empty()
        && missing_object_ids.is_empty()
        && missing_collations.is_empty()
    {
        return Ok(());
    }
    // Install the complete provisional registry so nested legacy views can derive each other's schemas while exact routine identities are bound and persisted in the current format.
    let previous_views = {
        let mut loaded = context.registry.views_write();
        std::mem::replace(&mut **loaded, views.clone())
    };
    let migration = (|| -> StorageBackendResult<()> {
        migrate_bound_definitions(
            context,
            views,
            routine_binding_migrations,
            missing_collations,
        )?;
        for relation in missing_output_columns {
            let view_name = relation.qualified_name();
            let output_columns = {
                let view = views.get(relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "legacy view `{view_name}` disappeared while restoring column metadata"
                    ))
                })?;
                let schema = context.schemas.stored_schema(view).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "restore legacy view `{view_name}` column metadata: {error}"
                    ))
                })?;
                uqa_sql::catalog::stored_view::restoration::restored_view_output_columns(&schema)
            };
            let view = views.get_mut(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "legacy view `{view_name}` disappeared while installing column metadata"
                ))
            })?;
            view.output_columns = Some(output_columns);
            context
                .registry
                .views_write()
                .insert(relation.clone(), view.clone());
        }
        let migrated_views = routine_binding_migrations
            .iter()
            .chain(missing_output_columns)
            .chain(missing_object_ids)
            .chain(missing_collations)
            .cloned()
            .collect::<BTreeSet<_>>();
        for relation in migrated_views {
            let view_name = relation.qualified_name();
            let view = views.get(&relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "legacy view `{view_name}` disappeared during migration"
                ))
            })?;
            catalog
                .save_view(&catalog_view_row(&relation, view)?)
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "migrate view `{view_name}` metadata: {error}"
                    ))
                })?;
        }
        Ok(())
    })();
    if let Err(error) = migration {
        **context.registry.views_write() = previous_views;
        return Err(error);
    }
    Ok(())
}

fn migrate_bound_definitions(
    context: &ViewRestoreContext<'_>,
    views: &mut BTreeMap<RelationIdentity, StoredView>,
    routine_binding_migrations: &BTreeSet<RelationIdentity>,
    missing_collations: &BTreeSet<RelationIdentity>,
) -> StorageBackendResult<()> {
    // Stored queries are bound as they were created: their set-returning calls are not placed again, so a view stored by an earlier release keeps loading.
    let _stored = SetPlacementScope::stored_definition();
    let binding_targets = routine_binding_migrations
        .union(missing_collations)
        .cloned()
        .collect();
    let ordered = uqa_sql::catalog::stored_view::restoration::view_metadata_dependency_order(
        views,
        &binding_targets,
    )
    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    for relation in &ordered {
        let view_name = relation.qualified_name();
        let view = views.get_mut(relation).ok_or_else(|| {
            StorageBackendError::Other(format!(
                "view `{view_name}` disappeared during metadata migration"
            ))
        })?;
        let schema = context
            .bindings
            .bind_routines(&mut view.query, &[])
            .map_err(|error| {
                StorageBackendError::Other(format!("restore view `{view_name}` bindings: {error}"))
            })?;
        // A query stored by an earlier release records no input collations; derive them as creation does.
        if routine_binding_migrations.contains(relation) {
            context
                .bindings
                .assign_input_collations(&mut view.query)
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "restore view `{view_name}` input collations: {error}"
                    ))
                })?;
        }
        if missing_collations.contains(relation) {
            view.restore_materialized_collations(&schema)
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "restore view `{view_name}` collations: {error}"
                    ))
                })?;
        }
        context
            .registry
            .views_write()
            .insert(relation.clone(), view.clone());
    }
    Ok(())
}
