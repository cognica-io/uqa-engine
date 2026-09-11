//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist foreign definition candidates before publishing their live catalog registries.
use crate::catalog::foreign::StoredForeignTable;
use crate::schema::{
    events::context::EventLifecycleContext,
    foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges,
    view_references::{self, ViewReferenceContext},
};
use uqa_core::RelationIdentity;
use uqa_sql::schema::foreign_tables::dependencies as declarations;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};
pub mod migration;
pub struct ForeignDefinitionContext<'a> {
    pub registry: &'a dyn ForeignRegistryReads,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub views: ViewReferenceContext<'a>,
    pub events: EventLifecycleContext<'a>,
}
impl ForeignDefinitionContext<'_> {
    pub fn persist_foreign_table_definition(
        &self,
        relation: &RelationIdentity,
        table: &StoredForeignTable,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.catalog else {
            return Ok(());
        };
        let security = self
            .registry
            .security()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        catalog.save_foreign_table(&table.catalog_row(relation, &security)?)
    }

    fn update_foreign_table_definition(
        &self,
        table_name: &str,
        update: impl FnOnce(&mut StoredForeignTable) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.registry.tables().get(&relation).cloned() else {
            return Ok(None);
        };
        if !update(&mut table)? {
            return Ok(Some(false));
        }
        self.persist_foreign_table_definition(&relation, &table)?;
        self.publication.tables_write().insert(relation, table);
        self.changes.catalog_registry_changed();
        Ok(Some(true))
    }

    pub fn clear_foreign_table_default_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            Ok(declarations::clear_foreign_column_default(
                &mut table.columns,
                column_name,
            ))
        })
    }

    pub fn drop_foreign_table_check_dependency(
        &self,
        table_name: &str,
        constraint_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            Ok(declarations::remove_foreign_check(
                &mut table.columns,
                &mut table.checks,
                constraint_name,
            ))
        })
    }

    pub fn drop_foreign_table_column_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.registry.tables().get(&relation).cloned() else {
            return Ok(None);
        };
        let Some(column_index) = table
            .columns
            .iter()
            .position(|column| column.name == column_name)
        else {
            return Ok(Some(false));
        };
        let dependent_views =
            view_references::views_depending_on_column(&self.views, table_name, column_name)?;
        if !dependent_views.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "cannot drop generated column `{table_name}`.`{column_name}` while dependent view(s) `{}` remain",
                dependent_views.join("`, `")
            )));
        }
        declarations::validate_foreign_column_removal(&table.columns, table_name, column_name)
            .map_err(StorageBackendError::Other)?;
        self.events
            .handle_drop_column_event_dependencies(table_name, column_name, false)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        declarations::remove_foreign_column(
            &mut table.columns,
            &mut table.checks,
            column_index,
            column_name,
        );
        let mut security = self
            .registry
            .security()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{table_name}` has no security metadata"
                ))
            })?;
        security.column_acls.remove(column_name);
        if let Some(catalog) = self.catalog {
            catalog.save_foreign_table(&table.catalog_row(&relation, &security)?)?;
        }
        self.publication
            .tables_write()
            .insert(relation.clone(), table);
        self.publication.security_write().insert(relation, security);
        self.changes.catalog_registry_changed();
        Ok(Some(true))
    }

    pub fn detach_foreign_table_sequence_provenance(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<bool> {
        let mut updates = Vec::new();
        for (relation, mut table) in self.registry.tables().clone() {
            let changed =
                declarations::detach_foreign_sequence_provenance(&mut table.columns, sequence);
            if changed {
                updates.push((relation, table));
            }
        }
        for (relation, table) in &updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        let changed = !updates.is_empty();
        if changed {
            let mut tables = self.publication.tables_write();
            for (relation, table) in updates {
                tables.insert(relation, table);
            }
            drop(tables);
            self.changes.catalog_registry_changed();
        }
        Ok(changed)
    }
}

use crate::catalog::foreign::reads::ForeignRegistryReads;
