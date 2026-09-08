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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogCacheRevisions {
    pub table_catalog: u64,
    pub registries: u64,
    pub table_data: BTreeMap<String, u64>,
    pub column_statistics: BTreeMap<String, u64>,
    pub statistics_maintenance: BTreeMap<String, u64>,
    /// Physical schema changes also invalidate bindings, even when a caller
    /// changed the storage schema without an ordinary catalog operation.
    pub storage_schema: u64,
}
