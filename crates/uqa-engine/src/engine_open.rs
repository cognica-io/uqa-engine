//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    initial_random_state, normalize_analyzer_phase, parse_analyzer_config, Analyzer, Arc,
    AtomicBool, BTreeMap, CatalogFacade, ColumnStatsRow, DeepModel, DurableCatalogState, Engine,
    EpochCoordinator, FieldName, IVFIndexParams, ManagedConnection, Path, PersistentStorageBackend,
    PersistentStorageProvider, PersistentStorageSession, QueryRuntime, RuntimeExtensions, RwLock,
    SQLiteCompressedContainerAnchor, SQLiteCompressionOptions, SQLiteError, SQLiteStorageProvider,
    SessionContext, StorageBackendError, StorageBackendResult, StorageContext, TableSchema,
    TableState, Value, VectorIndex, GRAPH_LABELS_METADATA_PREFIX, SQL_FUNCTION_DEPTH_LIMIT,
};

mod catalog_sync;
mod data_sync;
mod graphs;
mod lifecycle;
mod registries;
mod statistics;
mod table_restore;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogRestoreMode {
    InitialMigration,
    LoadOnly,
}

impl CatalogRestoreMode {
    pub(crate) fn allows_migration(self) -> bool {
        self == Self::InitialMigration
    }
}

#[cfg(test)]
mod tests;
