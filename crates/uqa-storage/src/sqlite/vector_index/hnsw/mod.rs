//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed HNSW index with a session-local immutable graph generation.

use std::sync::Arc;

use parking_lot::RwLock;

use super::{ManagedConnection, SQLiteResult, SQLiteVectorIndex};
use crate::hnsw_index::HNSWIndex;
use crate::vector_index::HNSWIndexParams;

mod consistency;
mod encoding;
mod lifecycle;
mod loading;
mod mutation;
mod persistence;
mod search;
mod writing;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct SQLiteHNSWIndex {
    pub(super) persistent: SQLiteVectorIndex,
    pub(super) params: HNSWIndexParams,
    pub(super) graph: Arc<RwLock<Option<CachedGraph>>>,
    pub(super) require_persisted_graph: bool,
}

#[derive(Clone)]
pub(super) struct CachedGraph {
    pub(super) revision: u64,
    pub(super) graph: Arc<HNSWIndex>,
}

impl SQLiteHNSWIndex {
    pub fn new(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self::with_params(conn, table, field, dimensions, HNSWIndexParams::default())
    }

    pub fn with_params(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> Self {
        Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params,
            graph: Arc::new(RwLock::new(None)),
            require_persisted_graph: false,
        }
    }

    pub fn open_existing(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> Self {
        let mut index = Self::with_params(conn, table, field, dimensions, params);
        index.require_persisted_graph = true;
        index
    }

    pub fn drop_metadata(conn: &ManagedConnection, table: &str, field: &str) -> SQLiteResult<()> {
        writing::drop_metadata(conn, table, field)
    }
}
