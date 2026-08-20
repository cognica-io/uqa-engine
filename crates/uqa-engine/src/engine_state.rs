//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ownership boundaries for storage, catalog, session, runtime, and epochs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;

use parking_lot::{Mutex, ReentrantMutex, RwLock};

use super::{
    BayesianBM25Params, CommandMutationOverlay, DeepModel, RegisteredSQLFunction, RelationIdentity,
    SQLAggregateFunction, SQLScalarFunction, SQLStatementCache, SQLTableFunction, SequenceState,
    TableFieldAnalyzerRegistry, TableState, TransactionFrame,
};

pub(super) struct StorageContext {
    pub(super) tables: RwLock<BTreeMap<RelationIdentity, Arc<TableState>>>,
    pub(super) catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
    pub(super) backend: Option<Arc<dyn uqa_storage::PersistentStorageBackend>>,
    pub(super) provider: Option<Arc<dyn uqa_storage::PersistentStorageProvider>>,
}

impl StorageContext {
    pub(super) fn memory() -> Self {
        Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: None,
            backend: None,
            provider: None,
        }
    }

    pub(super) fn persistent(
        catalog: Arc<dyn uqa_storage::CatalogFacade>,
        backend: Arc<dyn uqa_storage::PersistentStorageBackend>,
        provider: Option<Arc<dyn uqa_storage::PersistentStorageProvider>>,
    ) -> Self {
        Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog),
            backend: Some(backend),
            provider,
        }
    }
}

pub(super) struct DurableCatalogState {
    pub(super) graphs: RwLock<BTreeMap<String, uqa_graph::MemoryGraphStore>>,
    pub(super) models: RwLock<BTreeMap<String, DeepModel>>,
    pub(super) scoring_params: RwLock<BTreeMap<String, String>>,
    pub(super) views: RwLock<BTreeMap<RelationIdentity, uqa_planner::QueryPlan>>,
    pub(super) catalog_indexes: RwLock<BTreeMap<String, uqa_storage::CatalogIndexRow>>,
    pub(super) schemas: RwLock<BTreeSet<String>>,
    pub(super) path_indexes: RwLock<BTreeMap<String, uqa_graph::PathIndex>>,
    pub(super) sequences: RwLock<BTreeMap<RelationIdentity, SequenceState>>,
    pub(super) named_analyzers: RwLock<BTreeMap<String, String>>,
    pub(super) table_field_analyzers: RwLock<TableFieldAnalyzerRegistry>,
    pub(super) foreign_servers: RwLock<BTreeMap<String, uqa_fdw::ForeignServer>>,
    pub(super) foreign_tables: RwLock<BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>>,
    pub(super) sql_user_functions:
        RwLock<BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>>,
}

#[derive(Clone)]
pub(super) struct DurableCatalogSnapshot {
    graphs: BTreeMap<String, uqa_graph::MemoryGraphStore>,
    models: BTreeMap<String, DeepModel>,
    scoring_params: BTreeMap<String, String>,
    views: BTreeMap<RelationIdentity, uqa_planner::QueryPlan>,
    catalog_indexes: BTreeMap<String, uqa_storage::CatalogIndexRow>,
    schemas: BTreeSet<String>,
    path_indexes: BTreeMap<String, uqa_graph::PathIndex>,
    sequences: BTreeMap<RelationIdentity, SequenceState>,
    named_analyzers: BTreeMap<String, String>,
    table_field_analyzers: TableFieldAnalyzerRegistry,
    foreign_servers: BTreeMap<String, uqa_fdw::ForeignServer>,
    foreign_tables: BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>,
    sql_user_functions: BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>,
}

impl DurableCatalogState {
    pub(super) fn new() -> Self {
        Self {
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            catalog_indexes: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(BTreeSet::from(["public".to_string()])),
            path_indexes: RwLock::new(BTreeMap::new()),
            sequences: RwLock::new(BTreeMap::new()),
            named_analyzers: RwLock::new(BTreeMap::new()),
            table_field_analyzers: RwLock::new(BTreeMap::new()),
            foreign_servers: RwLock::new(BTreeMap::new()),
            foreign_tables: RwLock::new(BTreeMap::new()),
            sql_user_functions: RwLock::new(BTreeMap::new()),
        }
    }

    /// Capture durable registries in the canonical lock order used by memory
    /// transactions. The engine statement gate excludes concurrent mutation
    /// while this multi-registry snapshot is assembled.
    pub(super) fn snapshot(&self) -> DurableCatalogSnapshot {
        DurableCatalogSnapshot {
            graphs: self.graphs.read().clone(),
            models: self.models.read().clone(),
            scoring_params: self.scoring_params.read().clone(),
            views: self.views.read().clone(),
            catalog_indexes: self.catalog_indexes.read().clone(),
            schemas: self.schemas.read().clone(),
            path_indexes: self.path_indexes.read().clone(),
            sequences: self.sequences.read().clone(),
            named_analyzers: self.named_analyzers.read().clone(),
            table_field_analyzers: self.table_field_analyzers.read().clone(),
            foreign_servers: self.foreign_servers.read().clone(),
            foreign_tables: self.foreign_tables.read().clone(),
            sql_user_functions: self.sql_user_functions.read().clone(),
        }
    }

    /// Restore in the same canonical lock order as [`Self::snapshot`].
    pub(super) fn restore(&self, snapshot: &DurableCatalogSnapshot) {
        *self.graphs.write() = snapshot.graphs.clone();
        *self.models.write() = snapshot.models.clone();
        *self.scoring_params.write() = snapshot.scoring_params.clone();
        *self.views.write() = snapshot.views.clone();
        *self.catalog_indexes.write() = snapshot.catalog_indexes.clone();
        self.schemas.write().clone_from(&snapshot.schemas);
        *self.path_indexes.write() = snapshot.path_indexes.clone();
        *self.sequences.write() = snapshot.sequences.clone();
        *self.named_analyzers.write() = snapshot.named_analyzers.clone();
        *self.table_field_analyzers.write() = snapshot.table_field_analyzers.clone();
        *self.foreign_servers.write() = snapshot.foreign_servers.clone();
        *self.foreign_tables.write() = snapshot.foreign_tables.clone();
        *self.sql_user_functions.write() = snapshot.sql_user_functions.clone();
    }
}

pub(super) struct SessionContext {
    /// Transactional session values share one lock so snapshots and restores
    /// cannot observe a mixture of old and new search-path, PRNG, sequence,
    /// prepared-plan, or statement-cache state.
    pub(super) state: RwLock<super::SessionStateSnapshot>,
    pub(super) transactions: Mutex<Vec<TransactionFrame>>,
    /// One row-lock recheck context per in-flight SQL statement. Query-bearing commands, prepared execution, and `EXPLAIN ANALYZE` spawn nested plan executors that must share the outermost statement's context, while a host-callback statement nested inside another statement owns its own frame.
    pub(super) row_lock_statements:
        Mutex<Vec<Option<std::sync::Arc<crate::sql::RowLockRetryCache>>>>,
    pub(super) command_mutation_overlays: Mutex<Vec<CommandMutationOverlay>>,
}

impl SessionContext {
    pub(super) fn new(random_state: u64) -> Self {
        let state = super::SessionStateSnapshot {
            search_path: vec!["public".to_string()],
            session_vars: BTreeMap::new(),
            random_state,
            sequence_currvals: BTreeMap::new(),
            prepared: BTreeMap::new(),
            sql_statement_cache: SQLStatementCache::default(),
        };
        Self {
            state: RwLock::new(state),
            transactions: Mutex::new(Vec::new()),
            row_lock_statements: Mutex::new(Vec::new()),
            command_mutation_overlays: Mutex::new(Vec::new()),
        }
    }
}

pub(super) struct RuntimeExtensions {
    pub(super) foreign_memory_tables: Arc<RwLock<BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>>>,
    pub(super) scalar_functions:
        Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLScalarFunction>>>>,
    pub(super) table_functions:
        Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLTableFunction>>>>,
    pub(super) aggregate_functions:
        Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLAggregateFunction>>>>,
}

impl RuntimeExtensions {
    pub(super) fn new() -> Self {
        Self {
            foreign_memory_tables: Arc::new(RwLock::new(BTreeMap::new())),
            scalar_functions: Arc::new(RwLock::new(BTreeMap::new())),
            table_functions: Arc::new(RwLock::new(BTreeMap::new())),
            aggregate_functions: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(super) fn shared_from(source: &Self) -> Self {
        Self {
            foreign_memory_tables: source.foreign_memory_tables.clone(),
            scalar_functions: source.scalar_functions.clone(),
            table_functions: source.table_functions.clone(),
            aggregate_functions: source.aggregate_functions.clone(),
        }
    }
}

pub(super) struct QueryRuntime {
    pub(super) statement_gate: ReentrantMutex<()>,
    pub(super) cancellation: uqa_core::CancellationToken,
    pub(super) notices: Mutex<Vec<(String, String)>>,
    pub(super) function_depth_limit: AtomicUsize,
    pub(super) bayesian_params_cache: RwLock<BTreeMap<String, BayesianBM25Params>>,
}

impl QueryRuntime {
    pub(super) fn new(function_depth_limit: usize) -> Self {
        Self {
            statement_gate: ReentrantMutex::new(()),
            cancellation: uqa_core::CancellationToken::new(),
            notices: Mutex::new(Vec::new()),
            function_depth_limit: AtomicUsize::new(function_depth_limit),
            bayesian_params_cache: RwLock::new(BTreeMap::new()),
        }
    }
}

pub(super) struct EpochChannel {
    pub(super) published: Arc<AtomicU64>,
    pub(super) seen: AtomicU64,
    pub(super) dirty: AtomicBool,
    pub(super) refresh: Mutex<()>,
}

impl EpochChannel {
    fn new(initial: u64) -> Self {
        Self {
            published: Arc::new(AtomicU64::new(initial)),
            seen: AtomicU64::new(initial),
            dirty: AtomicBool::new(false),
            refresh: Mutex::new(()),
        }
    }
}

pub(super) struct EpochCoordinator {
    pub(super) seen_storage_change_version: AtomicU64,
    pub(super) external_commit_refresh: Mutex<()>,
    pub(super) table_catalog: EpochChannel,
    pub(super) table_data: EpochChannel,
    pub(super) catalog_registry: EpochChannel,
}

impl EpochCoordinator {
    pub(super) fn new() -> Self {
        Self {
            seen_storage_change_version: AtomicU64::new(0),
            external_commit_refresh: Mutex::new(()),
            table_catalog: EpochChannel::new(1),
            table_data: EpochChannel::new(1),
            catalog_registry: EpochChannel::new(1),
        }
    }

    pub(super) fn share_published_from(&mut self, source: &Self) {
        self.table_catalog.published = source.table_catalog.published.clone();
        self.table_data.published = source.table_data.published.clone();
        self.catalog_registry.published = source.catalog_registry.published.clone();
        self.table_catalog
            .seen
            .store(0, std::sync::atomic::Ordering::Release);
        self.table_data
            .seen
            .store(0, std::sync::atomic::Ordering::Release);
        self.catalog_registry
            .seen
            .store(0, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
