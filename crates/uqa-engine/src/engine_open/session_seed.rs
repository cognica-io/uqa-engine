//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind independent physical sessions to immutable committed catalog definitions.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use uqa_storage::{PersistentStorageProvider, PersistentStorageSession, StorageBackendResult};

use crate::engine_state::CatalogCell;
use crate::{Engine, TableState, VectorIndexOpenMode, VectorIndexSpec};

impl Engine {
    pub(super) fn session_from_shared_catalog(
        &self,
        storage: &PersistentStorageSession,
        provider: &Arc<dyn PersistentStorageProvider>,
    ) -> StorageBackendResult<Option<Self>> {
        let source_backend = self
            .storage
            .backend
            .as_ref()
            .expect("persistent session source");
        let before = source_backend.change_version()?;
        if !before.is_some_and(|version| {
            version
                == self
                    .epochs
                    .seen_storage_change_version
                    .load(Ordering::Acquire)
        }) {
            return Ok(None);
        }
        let epochs = self.epochs.published_epochs();
        let cache_revisions = self.epochs.storage_cache_revisions.lock().clone();
        let session = Self::empty_persistent_session(
            PersistentStorageSession::new(
                Arc::clone(&storage.catalog),
                Arc::clone(&storage.backend),
            ),
            Some(Arc::clone(provider)),
        );
        session.durable.restore(&self.durable.snapshot());
        session.rebind_graph_stores()?;
        for (relation, source) in self.storage.tables.read().iter() {
            if source.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let name = relation.qualified_name();
            let table = Self::bind_shared_session_table(storage, &name, source)?;
            session
                .storage
                .tables
                .write()
                .insert(relation.clone(), Arc::clone(&table));
            session.rebind_persistent_table_stores(&name, &table)?;
        }
        // Compare the source monitor with itself: providers need not use
        // comparable generation numbers across independent monitor handles.
        let session_version = storage.backend.change_version()?;
        if before != source_backend.change_version()? || epochs != self.epochs.published_epochs() {
            return Ok(None);
        }
        if let Some(version) = session_version {
            session
                .epochs
                .seen_storage_change_version
                .store(version, Ordering::Release);
        }
        *session.epochs.storage_cache_revisions.lock() = cache_revisions;
        Ok(Some(session))
    }

    fn bind_shared_session_table(
        storage: &PersistentStorageSession,
        name: &str,
        source: &TableState,
    ) -> StorageBackendResult<Arc<TableState>> {
        let analyzer = source.analyzer.read().clone();
        let mut vectors = std::collections::BTreeMap::new();
        for (field, index) in source.vector_indexes.read().iter() {
            vectors.insert(
                field.clone(),
                storage.backend.vector_index(
                    name,
                    field,
                    index.dimensions(),
                    VectorIndexSpec::BruteForce,
                    VectorIndexOpenMode::Restore,
                )?,
            );
        }
        Ok(Arc::new(TableState {
            lifecycle_id: AtomicU64::new(crate::next_table_lifecycle_id()),
            object_id: source.object_id(),
            security: CatalogCell::from_snapshot(source.security.snapshot()),
            storage_generation: RwLock::new(source.storage_generation()),
            document_store: RwLock::new(storage.backend.document_store(name)),
            inverted_index: RwLock::new(storage.backend.inverted_index(name, analyzer)),
            vector_indexes: RwLock::new(vectors),
            fts_fields: CatalogCell::from_snapshot(source.fts_fields.snapshot()),
            columns: CatalogCell::from_snapshot(source.columns.snapshot()),
            columns_declared: CatalogCell::from_snapshot(source.columns_declared.snapshot()),
            next_id: Mutex::new(*source.next_id.lock()),
            analyzer: CatalogCell::from_snapshot(source.analyzer.snapshot()),
            column_stats: CatalogCell::from_snapshot(source.column_stats.snapshot()),
            column_stats_loaded: AtomicBool::new(
                source.column_stats_loaded.load(Ordering::Acquire),
            ),
            column_stats_dirty: AtomicBool::new(source.column_stats_dirty.load(Ordering::Acquire)),
            table_checks: CatalogCell::from_snapshot(source.table_checks.snapshot()),
            foreign_keys: CatalogCell::from_snapshot(source.foreign_keys.snapshot()),
            key_constraints: CatalogCell::from_snapshot(source.key_constraints.snapshot()),
            hierarchy: CatalogCell::from_snapshot(source.hierarchy.snapshot()),
            value_indexes: RwLock::new(std::collections::BTreeMap::new()),
            doc_count_cache: AtomicU64::new(source.doc_count_cache.load(Ordering::Acquire)),
            doc_count_dirty: AtomicBool::new(source.doc_count_dirty.load(Ordering::Acquire)),
            persistence: source.persistence,
            on_commit: source.on_commit,
        }))
    }
}
