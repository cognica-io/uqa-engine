//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind maintenance to retained table generations, session locks and provider transactions.
use crate::{Engine, TableState};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::Ordering, Arc},
};
use uqa_core::DocId;
use uqa_execution::{
    maintenance::{
        VacuumContext, VacuumDocuments, VacuumLocks, VacuumRelations, VacuumRows, VacuumStatistics,
        VacuumStatisticsRefresh, VacuumStorage, VacuumTable, VacuumTransactions,
        VacuumVectorIndexes, VacuumWrite,
    },
    mutation::publication::DocumentVectors,
};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::security::table::TableAclPrivilege,
    maintenance::{VacuumCatalog, VacuumPrivileges, VacuumRelation},
    SQLError,
};
use uqa_storage::{document_store::Document, StorageBackendResult, StoredDocument};
impl Engine {
    pub(crate) fn vacuum_execution_context(&self) -> VacuumContext<'_> {
        VacuumContext {
            catalog: self,
            privileges: self,
            relations: self,
            locks: self,
            storage: self,
            rows: self,
            statistics: self,
            transactions: self,
        }
    }
}
struct VacuumMetadata(Arc<TableState>);
impl VacuumRelation for VacuumMetadata {
    fn column_names(&self) -> BTreeSet<String> {
        self.0
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect()
    }
}
impl VacuumCatalog for Engine {
    fn resolve_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.try_resolve_visible_relation_kind(name)
    }
    fn require_table(&self, name: &str) -> Result<Box<dyn VacuumRelation + '_>, SQLError> {
        Engine::require_table(self, name)
            .map(|state| Box::new(VacuumMetadata(state)) as Box<dyn VacuumRelation>)
    }
}
impl VacuumPrivileges for Engine {
    fn ensure_maintain(&self, table: &str) -> Result<(), SQLError> {
        self.ensure_table_privilege(table, TableAclPrivilege::Maintain)
    }
}
struct VacuumTableBinding<'a> {
    engine: &'a Engine,
    table: Arc<TableState>,
}
struct SavedVacuumStatistics<'a> {
    engine: &'a Engine,
    table: &'a TableState,
    statistics: BTreeMap<String, uqa_planner::ColumnStats>,
    loaded: bool,
    dirty: bool,
}
impl VacuumStatistics for SavedVacuumStatistics<'_> {
    fn loaded(&self) -> bool {
        self.loaded
    }
    fn dirty(&self) -> bool {
        self.dirty
    }
    fn restore(&self) {
        *self.table.column_stats.write() = self.statistics.clone();
        self.table
            .column_stats_loaded
            .store(self.loaded, Ordering::Release);
        self.table
            .column_stats_dirty
            .store(self.dirty, Ordering::Release);
    }
    fn persist(&self, table: &str) -> StorageBackendResult<()> {
        if let Some(catalog) = self.engine.storage.catalog.as_ref() {
            Engine::persist_column_stats(catalog.as_ref(), table, &self.statistics)?;
        }
        Ok(())
    }
}
impl VacuumRelations for Engine {
    fn require_table(&self, name: &str) -> Result<Box<dyn VacuumTable + '_>, SQLError> {
        Engine::require_table(self, name).map(|table| {
            Box::new(VacuumTableBinding {
                engine: self,
                table,
            }) as Box<dyn VacuumTable>
        })
    }
    fn scan_tables(&self, name: &str, descendants: bool) -> Result<Vec<String>, SQLError> {
        self.hierarchy_scan_tables(name, descendants)
    }
}
impl VacuumTable for VacuumTableBinding<'_> {
    fn statistics(&self) -> Box<dyn VacuumStatistics + '_> {
        let statistics = self.table.column_stats.read().clone();
        let loaded = self.table.column_stats_loaded.load(Ordering::Acquire);
        let dirty = self.table.column_stats_dirty.load(Ordering::Acquire);
        Box::new(SavedVacuumStatistics {
            engine: self.engine,
            table: &self.table,
            statistics,
            loaded,
            dirty,
        })
    }
    fn documents(&self) -> VacuumDocuments<'_> {
        Box::new(self.table.document_store.read())
    }
    fn document_vectors(&self, document: &Document) -> Result<DocumentVectors, SQLError> {
        Engine::document_vector_values(&self.table, document)
    }
    fn clear_documents(&self) -> StorageBackendResult<()> {
        self.table.document_store.write().clear()
    }
    fn clear_text_index(&self) -> StorageBackendResult<()> {
        self.table.inverted_index.write().clear()
    }
    fn vector_indexes(&self) -> VacuumVectorIndexes<'_> {
        Box::new(self.table.vector_indexes.write())
    }
    fn clear_value_indexes(&self) {
        self.table.value_indexes.write().clear();
    }
    fn persistence(&self) -> RelationPersistence {
        self.table.persistence
    }
    fn mark_doc_count_dirty(&self) {
        self.table.doc_count_dirty.store(true, Ordering::Release);
    }
}
impl VacuumLocks for Engine {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError> {
        self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)
    }
    fn release_session(&self) {
        self.row_locks.release_session(self.session_id);
    }
}
impl VacuumStorage for Engine {
    fn vacuum(&self) -> StorageBackendResult<()> {
        if let Some(backend) = self.storage.backend.as_ref() {
            backend.vacuum()?;
        }
        Ok(())
    }
    fn clear_btree_indexes(&self, table: &str) -> StorageBackendResult<()> {
        if let Some(backend) = self.storage.backend.as_ref() {
            backend.clear_btree_indexes(table)?;
        }
        Ok(())
    }
}
impl VacuumRows for Engine {
    fn restore_document(
        &self,
        table: &str,
        id: DocId,
        document: StoredDocument,
        vectors: DocumentVectors,
    ) -> Result<(), SQLError> {
        self.add_prepared_stored_document_with_vector_values_inner(
            table, id, document, vectors, true,
        )
    }
    fn refresh_indexes(&self, table: &str) -> StorageBackendResult<()> {
        self.refresh_value_indexes_for_table(table)
    }
    fn note_table_data_changed(&self) {
        Engine::note_table_data_changed(self);
    }
}
impl VacuumStatisticsRefresh for Engine {
    fn table_names(&self, operation: &str) -> Result<Vec<String>, SQLError> {
        self.maintenance_table_names(operation)
    }
    fn analyze_target(
        &self,
        table: &str,
        columns: &[String],
        descendants: bool,
    ) -> StorageBackendResult<()> {
        self.run_analyze_target(table, columns, descendants)
    }
}
impl VacuumTransactions for Engine {
    fn depth(&self) -> usize {
        self.transaction_depth()
    }
    fn with_maintenance_write(&self, write: VacuumWrite<'_>) -> StorageBackendResult<()> {
        self.with_read_only_compatible_storage_transaction(|engine| {
            write(&engine.vacuum_execution_context())
        })
    }
}
