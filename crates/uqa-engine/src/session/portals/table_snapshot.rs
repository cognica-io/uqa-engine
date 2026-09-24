//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained table state keeps the selected storage owner's snapshot handles.

use super::{DocumentStore, Engine, InvertedIndex, SQLError, TableState};
use uqa_storage::ReadOnlySnapshot;

impl Engine {
    pub(crate) fn query_retention_control(
        &self,
    ) -> Result<uqa_storage::read_control::StorageReadControl, SQLError> {
        use uqa_storage::read_control::StorageReadControl;
        if let Some(control) = self
            .storage
            .backend
            .as_ref()
            .and_then(|backend| backend.retention_control())
        {
            return Ok(StorageReadControl::new(
                control.memory(),
                &self.runtime.cancellation,
            ));
        }
        if self.versioned_backend_transactions() {
            return Err(SQLError::Internal(
                "versioned backend omitted its retention allowance".into(),
            ));
        }
        Ok(StorageReadControl::new(
            &self.session.query_retention,
            &self.runtime.cancellation,
        ))
    }

    pub(crate) fn capture_query_document_changes(
        &self,
        table: &TableState,
        desired: uqa_execution::query::document_changes::DocumentSelection,
    ) -> Result<uqa_execution::query::document_changes::DocumentChanges, SQLError> {
        use uqa_execution::query::document_changes::DocumentChanges;
        use uqa_execution::storage_errors::storage_error;
        let control = self.query_retention_control()?;
        let source = table.document_store.read();
        if self.storage.backend.is_none() || self.versioned_backend_transactions() {
            DocumentChanges::default().with_retained(
                source
                    .snapshot()
                    .map_err(|error| storage_error("capture private document source", &error))?,
                desired,
                &control,
            )
        } else {
            DocumentChanges::capture_owned(source.as_ref(), desired, &control)
        }
        .map_err(|error| storage_error("capture private document changes", &error))
    }

    pub(super) fn with_query_snapshot_schema<T>(
        metadata: &TableState,
        operation: impl FnOnce(
            &uqa_execution::query::table_snapshot::SnapshotSchema<'_>,
        ) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let columns = metadata.columns.snapshot();
        let text_fields = metadata.fts_fields.snapshot();
        let text_revisions = metadata.inverted_index.read();
        let vector_dimensions = metadata.vector_indexes.read();
        operation(&uqa_execution::query::table_snapshot::SnapshotSchema {
            columns,
            text_fields: &text_fields,
            text_revisions: text_revisions.as_ref(),
            vector_dimensions: &*vector_dimensions,
        })
    }

    pub(super) fn retain_query_table(
        data: &std::sync::Arc<TableState>,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> Result<std::sync::Arc<TableState>, SQLError> {
        let document_store = data
            .document_store
            .read()
            .snapshot()
            .map_err(|error| super::portal_snapshot_error("documents", &error))?;
        let inverted_index = data
            .inverted_index
            .read()
            .snapshot_with_control(control)
            .map_err(|error| super::portal_snapshot_error("inverted index", &error))?;
        let vector_indexes = uqa_execution::query::table_snapshot::retain_vector_indexes(
            &*data.vector_indexes.read(),
            control,
        )?;
        Ok(Self::query_table_with_storage(
            data,
            Box::new(ReadOnlySnapshot::new(document_store)),
            Box::new(ReadOnlySnapshot::new(inverted_index)),
            vector_indexes,
            data.doc_count_cache
                .load(std::sync::atomic::Ordering::Acquire),
            data.doc_count_dirty
                .load(std::sync::atomic::Ordering::Acquire),
        ))
    }

    pub(super) fn query_table_with_storage(
        metadata: &std::sync::Arc<TableState>,
        document_store: Box<dyn DocumentStore>,
        inverted_index: Box<dyn InvertedIndex>,
        vector_indexes: uqa_storage::vector_index::VectorIndexes,
        doc_count: u64,
        doc_count_dirty: bool,
    ) -> std::sync::Arc<TableState> {
        std::sync::Arc::new(TableState {
            lifecycle_id: std::sync::atomic::AtomicU64::new(metadata.lifecycle_id()),
            object_id: metadata.object_id(),
            security: crate::state::CatalogCell::new(metadata.security()),
            storage_generation: parking_lot::RwLock::new(metadata.storage_generation()),
            document_store: parking_lot::RwLock::new(document_store),
            inverted_index: parking_lot::RwLock::new(inverted_index),
            vector_indexes: parking_lot::RwLock::new(vector_indexes),
            fts_fields: crate::state::CatalogCell::from_snapshot(metadata.fts_fields.snapshot()),
            columns: crate::state::CatalogCell::from_snapshot(metadata.columns.snapshot()),
            columns_declared: crate::state::CatalogCell::from_snapshot(
                metadata.columns_declared.snapshot(),
            ),
            next_id: parking_lot::Mutex::new(*metadata.next_id.lock()),
            analyzer: crate::state::CatalogCell::from_snapshot(metadata.analyzer.snapshot()),
            column_stats: crate::state::CatalogCell::from_snapshot(
                metadata.column_stats.snapshot(),
            ),
            column_stats_loaded: std::sync::atomic::AtomicBool::new(
                metadata
                    .column_stats_loaded
                    .load(std::sync::atomic::Ordering::Acquire),
            ),
            column_stats_dirty: std::sync::atomic::AtomicBool::new(
                metadata
                    .column_stats_dirty
                    .load(std::sync::atomic::Ordering::Acquire),
            ),
            table_checks: crate::state::CatalogCell::from_snapshot(
                metadata.table_checks.snapshot(),
            ),
            foreign_keys: crate::state::CatalogCell::from_snapshot(
                metadata.foreign_keys.snapshot(),
            ),
            key_constraints: crate::state::CatalogCell::from_snapshot(
                metadata.key_constraints.snapshot(),
            ),
            hierarchy: crate::state::CatalogCell::from_snapshot(metadata.hierarchy.snapshot()),
            value_indexes: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(doc_count),
            doc_count_dirty: std::sync::atomic::AtomicBool::new(doc_count_dirty),
            persistence: metadata.persistence,
            on_commit: metadata.on_commit,
        })
    }
}
