//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend actual table generations and guard lifetimes to native DROP execution.
use crate::{Engine, TableState};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::schema::table_removal::context::{
    TableRemovalCatalog, TableRemovalContext, TableRemovalPublication,
};
use uqa_sql::{
    schema::removal::hierarchy::{
        HierarchyDropCatalog, HierarchyDropEntries, HierarchyDropRead, HierarchyDropTable,
        HierarchyDropTables,
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;
struct HierarchyTables<'a>(
    parking_lot::RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>,
);
impl HierarchyDropCatalog for Engine {
    fn tables(&self) -> Box<dyn HierarchyDropTables + '_> {
        Box::new(HierarchyTables(self.storage.tables.read()))
    }
}
impl HierarchyDropTables for HierarchyTables<'_> {
    fn iter(&self) -> HierarchyDropEntries<'_> {
        Box::new(
            self.0
                .iter()
                .map(|(identity, state)| (identity, state.as_ref() as &dyn HierarchyDropTable)),
        )
    }
}
impl HierarchyDropTable for TableState {
    fn hierarchy(&self) -> HierarchyDropRead<'_> {
        Box::new(self.hierarchy.read())
    }
}
impl TableRemovalCatalog for Engine {
    fn contains_relation(&self, relation: &RelationIdentity) -> bool {
        self.storage.tables.read().contains_key(relation)
    }
}
impl TableRemovalPublication for Engine {
    fn prune_constraint_modes(&self) -> Result<(), SQLError> {
        Engine::prune_constraint_modes(self)
    }
    fn remove_state(&self, name: &str, relation: &RelationIdentity) -> StorageBackendResult<()> {
        let table = self.storage.tables.read().get(relation).cloned();
        let temporary = table
            .as_ref()
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.drop_table_and_data(name)?;
                if let Some(table) = &table {
                    uqa_execution::catalog::definition_revision::remove_table(
                        catalog.as_ref(),
                        table.object_id(),
                    )?;
                }
                self.note_table_catalog_changed();
            }
        }
        let removed = self.storage.tables.write().remove(relation);
        if let Some(table) = removed {
            self.note_prepared_table_change(&table);
        }
        self.forget_constraint_transaction_relation(relation);
        self.clear_regtype_output_cache();
        if temporary {
            self.note_table_catalog_changed();
        }
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(t, _), _| t != name);
        self.durable
            .catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        Ok(())
    }
}
impl Engine {
    pub(crate) fn table_removal_context(&self) -> TableRemovalContext<'_> {
        TableRemovalContext {
            indexes: self.index_registry_context(),
            catalog: self,
            hierarchy: self,
            publication: self,
            routines: self.routine_removal_context(),
            events: self.event_lifecycle_context(),
            views: self.view_removal_context(),
            sequences: self.sequence_removal_context(),
        }
    }
}
