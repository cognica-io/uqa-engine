//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable view catalog restoration and metadata migration.

use std::collections::{BTreeMap, BTreeSet};

use crate::engine_open::CatalogRestoreMode;
use crate::{
    CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult, StoredView,
    StoredViewKind, ViewRow,
};

use super::{
    bind_stored_view_relations, catalog_view_row, upgrade_legacy_view_dispatches, RestoredView,
};
use crate::engine_session::view_binding::bind_query_plan_sequence_references;

fn validate_restored_view_object_ids(
    views: &BTreeMap<RelationIdentity, StoredView>,
) -> StorageBackendResult<()> {
    let mut object_ids = BTreeSet::new();
    for (relation, view) in views {
        if !object_ids.insert(view.object_id) {
            return Err(StorageBackendError::Other(format!(
                "view `{}` has a duplicate object identity",
                relation.qualified_name()
            )));
        }
    }
    Ok(())
}

impl Engine {
    fn migrate_persisted_views(
        &self,
        catalog: &dyn CatalogFacade,
        views: &mut BTreeMap<RelationIdentity, StoredView>,
        routine_binding_migrations: &BTreeSet<RelationIdentity>,
        missing_output_columns: &[RelationIdentity],
        missing_object_ids: &[RelationIdentity],
    ) -> StorageBackendResult<()> {
        if routine_binding_migrations.is_empty()
            && missing_output_columns.is_empty()
            && missing_object_ids.is_empty()
        {
            return Ok(());
        }
        // Install the complete provisional registry so nested legacy views can derive each other's schemas while exact routine identities are bound and persisted in the current format.
        let previous_views = {
            let mut loaded = self.durable.views.write();
            std::mem::replace(&mut *loaded, views.clone())
        };
        let migration = (|| -> StorageBackendResult<()> {
            for relation in routine_binding_migrations {
                let view_name = relation.qualified_name();
                let view = views.get_mut(relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "view `{view_name}` disappeared during routine-binding migration"
                    ))
                })?;
                crate::sql::bind_catalog_query_routines(self, &mut view.query, &[]).map_err(
                    |error| {
                        StorageBackendError::Other(format!(
                            "restore view `{view_name}` routine bindings: {error}"
                        ))
                    },
                )?;
                self.durable
                    .views
                    .write()
                    .insert(relation.clone(), view.clone());
            }
            for relation in missing_output_columns {
                let view_name = relation.qualified_name();
                let output_columns = {
                    let view = views.get(relation).ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "legacy view `{view_name}` disappeared while restoring column metadata"
                        ))
                    })?;
                    let schema = self.stored_view_schema(view).map_err(|error| {
                        StorageBackendError::Other(format!(
                            "restore legacy view `{view_name}` column metadata: {error}"
                        ))
                    })?;
                    schema
                        .columns()
                        .iter()
                        .enumerate()
                        .map(|(position, column)| {
                            schema.public_name(position).unwrap_or(column).to_string()
                        })
                        .collect::<Vec<_>>()
                };
                let view = views.get_mut(relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "legacy view `{view_name}` disappeared while installing column metadata"
                    ))
                })?;
                view.output_columns = Some(output_columns);
                self.durable
                    .views
                    .write()
                    .insert(relation.clone(), view.clone());
            }
            let migrated_views = routine_binding_migrations
                .iter()
                .chain(missing_output_columns)
                .chain(missing_object_ids)
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
            *self.durable.views.write() = previous_views;
            return Err(error);
        }
        Ok(())
    }

    fn validate_and_persist_restored_views(
        &self,
        catalog: &dyn CatalogFacade,
        views: &BTreeMap<RelationIdentity, StoredView>,
        migrated_views: &BTreeSet<RelationIdentity>,
        dispatch_upgraded_views: &[RelationIdentity],
    ) -> StorageBackendResult<()> {
        for (relation, view) in views {
            let output_columns = view.output_columns.as_deref().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "view `{}` has no public column metadata after catalog migration",
                    relation.qualified_name()
                ))
            })?;
            crate::engine_table_security::validate_table_security_invariants(
                &view.security(),
                Some(output_columns),
                &self.durable.roles.read(),
            )
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "view `{}` has invalid privilege metadata after migration: {error}",
                    relation.qualified_name()
                ))
            })?;
        }
        for relation in dispatch_upgraded_views {
            if migrated_views.contains(relation) {
                continue;
            }
            let view = views.get(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "dispatch-upgraded view `{}` disappeared during restoration",
                    relation.qualified_name()
                ))
            })?;
            catalog.save_view(&catalog_view_row(relation, view)?)?;
        }
        Ok(())
    }

    fn temporary_stored_views(&self) -> BTreeMap<RelationIdentity, StoredView> {
        self.durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
            .map(|(relation, view)| (relation.clone(), view.clone()))
            .collect()
    }

    fn restored_view_relation_namespace(
        &self,
        rows: &[ViewRow],
        temporary_views: &BTreeMap<RelationIdentity, StoredView>,
    ) -> StorageBackendResult<BTreeSet<RelationIdentity>> {
        let mut relations = self
            .storage
            .tables
            .read()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        relations.extend(self.durable.foreign_tables.read().keys().cloned());
        relations.extend(rows.iter().map(|row| row.relation.clone()));
        for (graph, store) in self.durable.graphs.read().iter() {
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

    fn validate_restored_view_security(
        &self,
        view_name: &str,
        view: &StoredView,
    ) -> StorageBackendResult<()> {
        if !self.durable.roles.read().contains_key(&view.role_owner) {
            return Err(StorageBackendError::Other(format!(
                "view `{view_name}` is owned by missing role `{}`",
                view.role_owner
            )));
        }
        crate::engine_table_security::validate_table_security_invariants(
            &view.security(),
            view.output_columns.as_deref(),
            &self.durable.roles.read(),
        )
        .map_err(|error| {
            StorageBackendError::Other(format!(
                "view `{view_name}` has invalid privilege metadata: {error}"
            ))
        })?;
        Ok(())
    }

    pub(crate) fn restore_views_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let rows = catalog.load_views()?;
        let temporary_views = self.temporary_stored_views();
        let relations = self.restored_view_relation_namespace(&rows, &temporary_views)?;

        let mut views = BTreeMap::new();
        let mut routine_binding_migrations = BTreeSet::new();
        let mut missing_output_columns = Vec::new();
        let mut missing_object_ids = Vec::new();
        let mut dispatch_upgraded_views = Vec::new();
        for row in rows {
            let view_name = row.relation.qualified_name();
            let mut view = match serde_json::from_str::<RestoredView>(&row.definition_json)? {
                RestoredView::Current(view) => view,
                RestoredView::Legacy(query) => {
                    routine_binding_migrations.insert(row.relation.clone());
                    StoredView {
                        object_id: [0; 16],
                        role_owner: row.role_owner.clone(),
                        acl: row.acl.clone(),
                        column_acls: row.column_acls.clone(),
                        query,
                        output_columns: None,
                        persistence: uqa_sql::ast::RelationPersistence::Permanent,
                        options: Vec::new(),
                        kind: StoredViewKind::View,
                        materialized_rows: Vec::new(),
                        materialized_column_types: Vec::new(),
                        populated: true,
                    }
                }
            };
            view.role_owner = row.role_owner;
            view.acl = row.acl;
            view.column_acls = row.column_acls;
            if view.object_id == [0; 16] {
                view.object_id = crate::new_view_object_id()?;
                missing_object_ids.push(row.relation.clone());
            }
            if view.kind == StoredViewKind::View && view.output_columns.is_none() {
                missing_output_columns.push(row.relation.clone());
            }
            self.validate_restored_view_security(&view_name, &view)?;
            if upgrade_legacy_view_dispatches(&mut view.query) {
                dispatch_upgraded_views.push(row.relation.clone());
            }
            if crate::engine_session::view_binding::query_plan_has_legacy_routine_identity(
                &view.query,
            ) {
                routine_binding_migrations.insert(row.relation.clone());
            }
            bind_stored_view_relations(&mut view.query, &relations).map_err(|error| {
                StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
            })?;
            bind_query_plan_sequence_references(&mut view.query, &mut |reference| {
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)
            })
            .map_err(|error| {
                StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
            })?;
            views.insert(row.relation, view);
        }
        views.extend(temporary_views);
        validate_restored_view_object_ids(&views)?;
        if !mode.allows_migration()
            && (!routine_binding_migrations.is_empty()
                || !missing_output_columns.is_empty()
                || !missing_object_ids.is_empty()
                || !dispatch_upgraded_views.is_empty())
        {
            return Err(StorageBackendError::Other(
                "view catalog requires an initial-open metadata migration".into(),
            ));
        }
        self.migrate_persisted_views(
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
        self.validate_and_persist_restored_views(
            catalog,
            &views,
            &migrated_views,
            &dispatch_upgraded_views,
        )?;
        *self.durable.views.write() = views;
        Ok(())
    }
}
