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
    TableChecksWrite, TableColumnsWrite, TableForeignKeysWrite, TableRemovalCatalog,
    TableRemovalContext, TableRemovalEntry, TableRemovalPublication, TableRemovalState,
    TableRemovalTransactions, TableRemovalWrite,
};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    schema::removal::{
        hierarchy::{
            HierarchyDropCatalog, HierarchyDropEntries, HierarchyDropRead, HierarchyDropTable,
            HierarchyDropTables,
        },
        tables::{
            TableChecksRead, TableColumnsRead, TableForeignKeysRead, TableKeysRead,
            TableRemovalMetadata,
        },
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
impl TableRemovalMetadata for TableState {
    fn columns(&self) -> TableColumnsRead<'_> {
        Box::new(self.columns.read())
    }
    fn table_checks(&self) -> TableChecksRead<'_> {
        Box::new(self.table_checks.read())
    }
    fn foreign_keys(&self) -> TableForeignKeysRead<'_> {
        Box::new(self.foreign_keys.read())
    }
    fn key_constraints(&self) -> TableKeysRead<'_> {
        Box::new(self.key_constraints.read())
    }
}
struct RemovalTable<'a> {
    engine: &'a Engine,
    name: String,
    state: Arc<TableState>,
}
impl TableRemovalMetadata for RemovalTable<'_> {
    fn columns(&self) -> TableColumnsRead<'_> {
        TableRemovalMetadata::columns(self.state.as_ref())
    }
    fn table_checks(&self) -> TableChecksRead<'_> {
        TableRemovalMetadata::table_checks(self.state.as_ref())
    }
    fn foreign_keys(&self) -> TableForeignKeysRead<'_> {
        TableRemovalMetadata::foreign_keys(self.state.as_ref())
    }
    fn key_constraints(&self) -> TableKeysRead<'_> {
        TableRemovalMetadata::key_constraints(self.state.as_ref())
    }
}
impl TableRemovalState for RemovalTable<'_> {
    fn object_id(&self) -> [u8; 16] {
        self.state.object_id()
    }
    fn columns_write(&self) -> TableColumnsWrite<'_> {
        Box::new(self.state.columns.write())
    }
    fn checks_write(&self) -> TableChecksWrite<'_> {
        Box::new(self.state.table_checks.write())
    }
    fn foreign_keys_write(&self) -> TableForeignKeysWrite<'_> {
        Box::new(self.state.foreign_keys.write())
    }
    fn persist_constraints(
        &self,
        columns: &[ColumnDef],
        checks: &[TableCheck],
        foreign_keys: &[ForeignKey],
        keys: &[TableKeyConstraint],
    ) -> StorageBackendResult<()> {
        self.engine.persist_constraint_candidate(
            &self.name,
            &self.state,
            columns,
            checks,
            foreign_keys,
            keys,
        )
    }
}
impl TableRemovalCatalog for Engine {
    fn relation_kind(&self, name: &str) -> StorageBackendResult<Option<(String, &'static str)>> {
        self.try_resolve_relation_kind(name)
    }
    fn table_entries(&self) -> Vec<TableRemovalEntry<'_>> {
        Engine::table_entries(self)
            .into_iter()
            .map(|(name, state)| {
                (
                    name.clone(),
                    Box::new(RemovalTable {
                        engine: self,
                        name,
                        state,
                    }) as Box<dyn TableRemovalState>,
                )
            })
            .collect()
    }
    fn contains_relation(&self, relation: &RelationIdentity) -> bool {
        self.storage.tables.read().contains_key(relation)
    }
}
impl TableRemovalTransactions for Engine {
    fn with_table_removal_write(&self, write: TableRemovalWrite<'_>) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| write(&engine.table_removal_context()))
    }
}
impl TableRemovalPublication for Engine {
    fn prune_constraint_modes(&self) -> Result<(), SQLError> {
        Engine::prune_constraint_modes(self)
    }
    fn remove_state(&self, name: &str, relation: &RelationIdentity) -> StorageBackendResult<()> {
        let temporary = self
            .storage
            .tables
            .read()
            .get(relation)
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.drop_table_and_data(name)?;
                self.note_table_catalog_changed();
            }
        }
        self.storage.tables.write().remove(relation);
        self.statistics.invalidate_column_stats(name);
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
            catalog: self,
            hierarchy: self,
            publication: self,
            transactions: self,
            routines: self.routine_removal_context(),
            events: self.event_lifecycle_context(),
            views: self.view_removal_context(),
            sequences: self.sequence_removal_context(),
        }
    }
}
