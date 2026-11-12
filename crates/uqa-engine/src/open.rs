//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    initial_random_state, Analyzer, Arc, BTreeMap, CatalogFacade, DeepModel, DurableCatalogState,
    Engine, EpochCoordinator, FieldName, ManagedConnection, Path, PersistentStorageBackend,
    PersistentStorageProvider, PersistentStorageSession, QueryRuntime, RuntimeExtensions,
    SQLiteCompressedContainerAnchor, SQLiteCompressionOptions, SQLiteError, SQLiteStorageProvider,
    SessionContext, StorageBackendError, StorageBackendResult, StorageContext, TableSchema,
    TableState, VectorIndex, SQL_FUNCTION_DEPTH_LIMIT,
};

mod authenticated_session;
mod cache_refresh;
mod catalog_reload;
mod catalog_sync;
mod data_sync;
mod graphs;
mod key_identities;
mod lifecycle;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod persistent_input;
mod registries;
mod session_seed;
mod statistics;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod table_input;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod type_input;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(crate) use persistent_input::PersistentEngineInput;
mod table_restore;
mod value_restoration;

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
