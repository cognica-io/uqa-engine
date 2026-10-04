//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-visible invalidation generations, without loading catalog payloads.

use std::collections::BTreeMap;

/// Lightweight generations read from the same storage snapshot as catalog and
/// row data. Providers must advance them atomically with the corresponding
/// mutation, including direct storage writes, and restore them on rollback.
/// Missing support is distinct from an empty, unchanged database.
/// Generations are opaque equality tokens, not clocks or counts. A logical
/// provider may use the domain at or above [`Self::PRIVATE_GENERATION_BASE`]
/// for transaction-private changes; rollback restores the prior tokens and
/// commit replaces them with durable generations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogCacheRevisions {
    pub table_catalog: u64,
    pub registries: u64,
    /// Per-graph generations, separate from SQL registries. `None` preserves
    /// conservative restoration for providers without graph-scoped tracking.
    pub graphs: Option<BTreeMap<String, u64>>,
    pub table_data: BTreeMap<String, u64>,
    pub column_statistics: BTreeMap<String, u64>,
    pub statistics_maintenance: BTreeMap<String, u64>,
    /// Physical schema changes also invalidate bindings, even when a caller
    /// changed the storage schema without an ordinary catalog operation.
    pub storage_schema: u64,
}

impl CatalogCacheRevisions {
    /// The first generation of the domain reserved for changes private to the reading transaction. Durable generations stay below it.
    pub const PRIVATE_GENERATION_BASE: u64 = 1 << 63;

    /// Whether `generation` identifies a change private to the reading transaction instead of a committed generation.
    pub fn is_private_generation(generation: u64) -> bool {
        generation >= Self::PRIVATE_GENERATION_BASE
    }
}
