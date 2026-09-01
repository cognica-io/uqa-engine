//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ownership boundaries for storage, catalog, session, runtime, and epochs.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, ReentrantMutex, RwLock};

use super::{
    BayesianBM25Params, CommandMutationOverlay, DeepModel, RegisteredSQLFunction, RelationIdentity,
    SQLAggregateFunction, SQLScalarFunction, SQLStatementCache, SQLTableFunction, SequenceState,
    TableFieldAnalyzerRegistry, TableState, TransactionFrame,
};

pub(super) struct StorageContext {
    pub(super) tables: Arc<RwLock<BTreeMap<RelationIdentity, Arc<TableState>>>>,
    pub(super) catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
    pub(super) backend: Option<Arc<dyn uqa_storage::PersistentStorageBackend>>,
    pub(super) provider: Option<Arc<dyn uqa_storage::PersistentStorageProvider>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceSecurity {
    pub(crate) role_owner: String,
    pub(crate) acl: Option<Vec<uqa_storage::SequenceAclEntry>>,
}

impl StorageContext {
    pub(super) fn memory() -> Self {
        Self {
            tables: Arc::new(RwLock::new(BTreeMap::new())),
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
            tables: Arc::new(RwLock::new(BTreeMap::new())),
            catalog: Some(catalog),
            backend: Some(backend),
            provider,
        }
    }

    pub(super) fn shared_from(source: &Self) -> Self {
        Self {
            tables: Arc::clone(&source.tables),
            catalog: source.catalog.clone(),
            backend: source.backend.clone(),
            provider: source.provider.clone(),
        }
    }
}

/// One bound view query together with the fixed public column names captured when the view was created. `None` only represents catalogs written before column metadata was persisted.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredView {
    pub(crate) query: uqa_planner::QueryPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output_columns: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) persistence: uqa_sql::ast::RelationPersistence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) options: Vec<(String, String)>,
    #[serde(default)]
    pub(crate) kind: StoredViewKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) materialized_rows: Vec<uqa_sql::ResultRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) materialized_column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    #[serde(default = "default_view_populated")]
    pub(crate) populated: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum StoredViewKind {
    #[default]
    View,
    Materialized,
}

const fn default_view_populated() -> bool {
    true
}

pub(super) struct DurableCatalogState {
    pub(super) graphs: RwLock<BTreeMap<String, uqa_graph::MemoryGraphStore>>,
    pub(super) models: RwLock<BTreeMap<String, DeepModel>>,
    pub(super) scoring_params: RwLock<BTreeMap<String, String>>,
    pub(super) views: RwLock<BTreeMap<RelationIdentity, StoredView>>,
    pub(super) catalog_indexes: RwLock<BTreeMap<String, uqa_storage::CatalogIndexRow>>,
    pub(super) schemas: RwLock<BTreeSet<String>>,
    pub(super) path_indexes: RwLock<BTreeMap<String, uqa_graph::PathIndex>>,
    pub(super) sequences: RwLock<BTreeMap<RelationIdentity, SequenceState>>,
    pub(super) sequence_object_ids: RwLock<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub(super) sequence_persistence:
        RwLock<BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>>,
    pub(super) sequence_security: RwLock<BTreeMap<RelationIdentity, SequenceSecurity>>,
    pub(super) named_analyzers: RwLock<BTreeMap<String, String>>,
    pub(super) table_field_analyzers: RwLock<TableFieldAnalyzerRegistry>,
    pub(super) foreign_servers: RwLock<BTreeMap<String, uqa_fdw::ForeignServer>>,
    pub(super) foreign_tables: RwLock<BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>>,
    pub(super) sql_user_functions:
        RwLock<BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>>,
    pub(super) roles: RwLock<BTreeMap<String, super::engine_roles::RoleDefinition>>,
    pub(super) role_memberships: RwLock<
        BTreeMap<super::engine_roles::RoleMembershipKey, super::engine_roles::RoleMembership>,
    >,
    pub(super) triggers: RwLock<
        BTreeMap<
            uqa_storage::RelationIdentity,
            BTreeMap<String, super::engine_events::StoredTrigger>,
        >,
    >,
    pub(super) rules: RwLock<
        BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, super::engine_events::StoredRule>>,
    >,
}

#[derive(Clone)]
pub(super) struct DurableCatalogSnapshot {
    pub(super) graphs: BTreeMap<String, uqa_graph::MemoryGraphStore>,
    pub(super) models: BTreeMap<String, DeepModel>,
    pub(super) scoring_params: BTreeMap<String, String>,
    pub(super) views: BTreeMap<RelationIdentity, StoredView>,
    pub(super) catalog_indexes: BTreeMap<String, uqa_storage::CatalogIndexRow>,
    pub(super) schemas: BTreeSet<String>,
    pub(super) path_indexes: BTreeMap<String, uqa_graph::PathIndex>,
    pub(super) sequences: BTreeMap<RelationIdentity, SequenceState>,
    pub(super) sequence_object_ids: BTreeMap<RelationIdentity, [u8; 16]>,
    pub(super) sequence_persistence: BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>,
    pub(super) sequence_security: BTreeMap<RelationIdentity, SequenceSecurity>,
    pub(super) named_analyzers: BTreeMap<String, String>,
    pub(super) table_field_analyzers: TableFieldAnalyzerRegistry,
    pub(super) foreign_servers: BTreeMap<String, uqa_fdw::ForeignServer>,
    pub(super) foreign_tables: BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>,
    pub(super) sql_user_functions:
        BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>,
    pub(super) roles: BTreeMap<String, super::engine_roles::RoleDefinition>,
    pub(super) role_memberships:
        BTreeMap<super::engine_roles::RoleMembershipKey, super::engine_roles::RoleMembership>,
    pub(super) triggers: BTreeMap<
        uqa_storage::RelationIdentity,
        BTreeMap<String, super::engine_events::StoredTrigger>,
    >,
    pub(super) rules:
        BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, super::engine_events::StoredRule>>,
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
            sequence_object_ids: RwLock::new(BTreeMap::new()),
            sequence_persistence: RwLock::new(BTreeMap::new()),
            sequence_security: RwLock::new(BTreeMap::new()),
            named_analyzers: RwLock::new(BTreeMap::new()),
            table_field_analyzers: RwLock::new(BTreeMap::new()),
            foreign_servers: RwLock::new(BTreeMap::new()),
            foreign_tables: RwLock::new(BTreeMap::new()),
            sql_user_functions: RwLock::new(BTreeMap::new()),
            roles: RwLock::new(BTreeMap::from([(
                "uqa".to_string(),
                super::engine_roles::RoleDefinition::bootstrap(),
            )])),
            role_memberships: RwLock::new(BTreeMap::new()),
            triggers: RwLock::new(BTreeMap::new()),
            rules: RwLock::new(BTreeMap::new()),
        }
    }

    /// Capture durable registries in the transaction coordinator's canonical lock order.
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
            sequence_object_ids: self.sequence_object_ids.read().clone(),
            sequence_persistence: self.sequence_persistence.read().clone(),
            sequence_security: self.sequence_security.read().clone(),
            named_analyzers: self.named_analyzers.read().clone(),
            table_field_analyzers: self.table_field_analyzers.read().clone(),
            foreign_servers: self.foreign_servers.read().clone(),
            foreign_tables: self.foreign_tables.read().clone(),
            sql_user_functions: self.sql_user_functions.read().clone(),
            roles: self.roles.read().clone(),
            role_memberships: self.role_memberships.read().clone(),
            triggers: self.triggers.read().clone(),
            rules: self.rules.read().clone(),
        }
    }

    /// Restore in the transaction coordinator's canonical lock order.
    pub(super) fn restore(&self, snapshot: &DurableCatalogSnapshot) {
        *self.graphs.write() = snapshot.graphs.clone();
        *self.models.write() = snapshot.models.clone();
        *self.scoring_params.write() = snapshot.scoring_params.clone();
        *self.views.write() = snapshot.views.clone();
        *self.catalog_indexes.write() = snapshot.catalog_indexes.clone();
        self.schemas.write().clone_from(&snapshot.schemas);
        *self.path_indexes.write() = snapshot.path_indexes.clone();
        *self.sequences.write() = snapshot.sequences.clone();
        *self.sequence_object_ids.write() = snapshot.sequence_object_ids.clone();
        *self.sequence_persistence.write() = snapshot.sequence_persistence.clone();
        *self.sequence_security.write() = snapshot.sequence_security.clone();
        *self.named_analyzers.write() = snapshot.named_analyzers.clone();
        *self.table_field_analyzers.write() = snapshot.table_field_analyzers.clone();
        *self.foreign_servers.write() = snapshot.foreign_servers.clone();
        *self.foreign_tables.write() = snapshot.foreign_tables.clone();
        *self.sql_user_functions.write() = snapshot.sql_user_functions.clone();
        *self.roles.write() = snapshot.roles.clone();
        *self.role_memberships.write() = snapshot.role_memberships.clone();
        *self.triggers.write() = snapshot.triggers.clone();
        *self.rules.write() = snapshot.rules.clone();
    }
}

pub(super) struct SessionContext {
    /// Transactional session values share one lock so snapshots and restores
    /// cannot observe a mixture of old and new search-path, sequence,
    /// prepared-plan, or statement-cache state.
    pub(super) state: RwLock<super::SessionStateSnapshot>,
    /// `PostgreSQL` sequence reservations are session-local and nontransactional. They are intentionally kept outside `SessionStateSnapshot` so rollback never rewinds consumption or restores blocks discarded by `ALTER SEQUENCE`.
    pub(super) sequence_caches:
        Mutex<BTreeMap<super::RelationIdentity, super::SessionSequenceCache>>,
    /// `PostgreSQL`'s session PRNG is not transactional: failed statements and
    /// transaction or savepoint rollback leave every consumed draw in place.
    pub(super) random_state: Mutex<super::SessionRandomState>,
    pub(super) transactions: Mutex<Vec<TransactionFrame>>,
    /// One row-lock recheck context per in-flight SQL statement. Query-bearing commands, prepared execution, and `EXPLAIN ANALYZE` spawn nested plan executors that must share the outermost statement's context, while a host-callback statement nested inside another statement owns its own frame.
    pub(super) row_lock_statements:
        Mutex<Vec<Option<std::sync::Arc<crate::sql::RowLockRetryCache>>>>,
    pub(super) command_mutation_overlays: Mutex<Vec<CommandMutationOverlay>>,
    pub(super) portals: Mutex<BTreeMap<String, super::SessionPortalState>>,
    pub(super) next_portal_id: Mutex<usize>,
    pub(super) next_portal_transaction_origin: Mutex<u64>,
}

impl SessionContext {
    pub(super) fn new(random_state: super::SessionRandomState) -> Self {
        let state = super::SessionStateSnapshot {
            search_path: vec!["public".to_string()],
            temporary_namespace_allocated: false,
            session_vars: BTreeMap::new(),
            sequence_currvals: BTreeMap::new(),
            last_sequence: None,
            prepared: BTreeMap::new(),
            sql_statement_cache: SQLStatementCache::default(),
            portal_names: BTreeSet::new(),
            current_user: "uqa".to_string(),
            session_user: "uqa".to_string(),
        };
        Self {
            state: RwLock::new(state),
            sequence_caches: Mutex::new(BTreeMap::new()),
            random_state: Mutex::new(random_state),
            transactions: Mutex::new(Vec::new()),
            row_lock_statements: Mutex::new(Vec::new()),
            command_mutation_overlays: Mutex::new(Vec::new()),
            portals: Mutex::new(BTreeMap::new()),
            next_portal_id: Mutex::new(1),
            next_portal_transaction_origin: Mutex::new(1),
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

thread_local! {
    static DELEGATED_STATEMENT_GATES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct StatementGate {
    mutex: ReentrantMutex<()>,
}

impl StatementGate {
    fn new() -> Self {
        Self {
            mutex: ReentrantMutex::new(()),
        }
    }

    pub(super) fn lock(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        let identity = std::ptr::from_ref(self) as usize;
        let delegated =
            DELEGATED_STATEMENT_GATES.with(|delegated| delegated.borrow().contains(&identity));
        (!delegated).then(|| self.mutex.lock())
    }

    pub(super) fn delegate_to_current_thread(&self) -> DelegatedStatementGate<'_> {
        let identity = std::ptr::from_ref(self) as usize;
        DELEGATED_STATEMENT_GATES.with(|delegated| delegated.borrow_mut().push(identity));
        DelegatedStatementGate { gate: self }
    }
}

pub(super) struct DelegatedStatementGate<'gate> {
    gate: &'gate StatementGate,
}

impl Drop for DelegatedStatementGate<'_> {
    fn drop(&mut self) {
        let identity = std::ptr::from_ref(self.gate) as usize;
        DELEGATED_STATEMENT_GATES.with(|delegated| {
            let removed = delegated.borrow_mut().pop();
            debug_assert_eq!(
                removed,
                Some(identity),
                "statement-gate delegation stack mismatch"
            );
        });
    }
}

pub(super) struct QueryRuntime {
    pub(super) statement_gate: Arc<StatementGate>,
    pub(super) sql_execution_depth: AtomicUsize,
    pub(super) cancellation: uqa_core::CancellationToken,
    pub(super) notices: Arc<Mutex<Vec<(String, String)>>>,
    pub(super) function_depth_limit: AtomicUsize,
    pub(super) bayesian_params_cache: RwLock<BTreeMap<String, BayesianBM25Params>>,
    pub(super) regtype_output_cache: Mutex<Option<Arc<crate::sql::RegtypeOutputCatalog>>>,
    pub(super) regtype_output_cache_revision: AtomicU64,
}

impl QueryRuntime {
    pub(super) fn new(function_depth_limit: usize) -> Self {
        Self {
            statement_gate: Arc::new(StatementGate::new()),
            sql_execution_depth: AtomicUsize::new(0),
            cancellation: uqa_core::CancellationToken::new(),
            notices: Arc::new(Mutex::new(Vec::new())),
            function_depth_limit: AtomicUsize::new(function_depth_limit),
            bayesian_params_cache: RwLock::new(BTreeMap::new()),
            regtype_output_cache: Mutex::new(None),
            regtype_output_cache_revision: AtomicU64::new(0),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublishedEpochs {
    table_catalog: u64,
    table_data: u64,
    catalog_registry: u64,
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

    pub(super) fn published_epochs(&self) -> PublishedEpochs {
        PublishedEpochs {
            table_catalog: self.table_catalog.published.load(Ordering::Acquire),
            table_data: self.table_data.published.load(Ordering::Acquire),
            catalog_registry: self.catalog_registry.published.load(Ordering::Acquire),
        }
    }

    pub(super) fn share_published_from(&mut self, source: &Self) {
        self.share_published_from_at(
            source,
            PublishedEpochs {
                table_catalog: 0,
                table_data: 0,
                catalog_registry: 0,
            },
        );
    }

    pub(super) fn share_published_from_at(&mut self, source: &Self, observed: PublishedEpochs) {
        self.table_catalog.published = source.table_catalog.published.clone();
        self.table_data.published = source.table_data.published.clone();
        self.catalog_registry.published = source.catalog_registry.published.clone();
        self.table_catalog
            .seen
            .store(observed.table_catalog, Ordering::Release);
        self.table_data
            .seen
            .store(observed.table_data, Ordering::Release);
        self.catalog_registry
            .seen
            .store(observed.catalog_registry, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
