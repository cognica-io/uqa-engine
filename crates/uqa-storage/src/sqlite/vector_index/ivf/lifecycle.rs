//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construction and public vector-index contract for `SQLite` IVF.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use super::writing::drop_metadata;
use super::SQLiteIVFIndex;
use crate::sqlite::vector_index::SQLiteVectorIndex;
use crate::sqlite::{ManagedConnection, Result as SQLiteResult};
use crate::vector_index::{IVFIndexParams, VectorIndex};
use crate::StorageBackendResult;

impl SQLiteIVFIndex {
    pub fn new(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self::from_params(conn, table, field, dimensions, IVFIndexParams::default())
    }

    pub fn with_params(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self::from_params(
            conn,
            table,
            field,
            dimensions,
            IVFIndexParams {
                nlist: nlist.max(1),
                nprobe: nprobe.max(1),
                train_threshold: train_threshold.max(1),
            },
        )
    }

    fn from_params(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> Self {
        Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params,
        }
    }

    pub fn open_existing(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self::with_params(
            conn,
            table,
            field,
            dimensions,
            nlist,
            nprobe,
            train_threshold,
        )
    }

    pub fn drop_metadata(conn: &ManagedConnection, table: &str, field: &str) -> SQLiteResult<()> {
        drop_metadata(conn, table, field)
    }
}

impl VectorIndex for SQLiteIVFIndex {
    fn dimensions(&self) -> u32 {
        self.persistent.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "sqlite-ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.replace_document(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace_document(doc_id, vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.delete_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.clear_index()
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.search_top_k(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.persistent.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.persistent.count()
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.initialize_metadata()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
}
