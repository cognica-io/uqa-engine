//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ownership boundaries for storage, catalog, session, runtime, and epochs.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, ReentrantMutex, RwLock};

mod catalog_cell;
pub(crate) use catalog_cell::CatalogCell;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DatabasePrivileges {
    pub(crate) connect: bool,
    pub(crate) create: bool,
    pub(crate) temporary: bool,
}

impl DatabasePrivileges {
    pub(crate) const ALL: Self = Self {
        connect: true,
        create: true,
        temporary: true,
    };

    pub(crate) const fn intersects(self, other: Self) -> bool {
        (self.connect && other.connect)
            || (self.create && other.create)
            || (self.temporary && other.temporary)
    }

    pub(crate) fn insert(&mut self, other: Self) {
        self.connect |= other.connect;
        self.create |= other.create;
        self.temporary |= other.temporary;
    }

    pub(crate) fn remove(&mut self, other: Self) {
        self.connect &= !other.connect;
        self.create &= !other.create;
        self.temporary &= !other.temporary;
    }

    pub(crate) const fn is_empty(self) -> bool {
        !self.connect && !self.create && !self.temporary
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DatabaseAclEntry {
    pub(crate) role: String,
    pub(crate) grantor: Option<String>,
    pub(crate) privileges: DatabasePrivileges,
    pub(crate) grant_options: DatabasePrivileges,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DatabaseSecurity {
    pub(crate) role_owner: String,
    pub(crate) acl: Option<Vec<DatabaseAclEntry>>,
}

impl DatabaseSecurity {
    pub(crate) fn bootstrap() -> Self {
        Self {
            role_owner: "uqa".into(),
            acl: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceSecurity {
    pub(crate) role_owner: String,
    pub(crate) acl: Option<Vec<uqa_storage::SequenceAclEntry>>,
}

/// Complete table-shaped relation security state. Ownership and ACL changes are published through one value so readers cannot observe a torn authorization state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableSecurity {
    pub(crate) role_owner: String,
    pub(crate) acl: Option<Vec<uqa_storage::TableAclEntry>>,
    pub(crate) column_acls: BTreeMap<String, Vec<uqa_storage::TableAclEntry>>,
}

impl TableSecurity {
    pub(crate) fn owner(role_owner: impl Into<String>) -> Self {
        Self {
            role_owner: role_owner.into(),
            acl: None,
            column_acls: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaSecurity {
    pub(crate) role_owner: String,
    pub(crate) acl: Option<Vec<uqa_storage::SchemaAclEntry>>,
}

impl SchemaSecurity {
    pub(crate) fn from_row(row: uqa_storage::SchemaRow) -> (String, Self) {
        (
            row.name,
            Self {
                role_owner: row.role_owner,
                acl: row.acl,
            },
        )
    }

    pub(crate) fn row(&self, name: impl Into<String>) -> uqa_storage::SchemaRow {
        uqa_storage::SchemaRow {
            name: name.into(),
            role_owner: self.role_owner.clone(),
            acl: self.acl.clone(),
        }
    }

    pub(crate) fn legacy(name: &str) -> Self {
        let (_, security) = Self::from_row(uqa_storage::SchemaRow::legacy(name));
        security
    }
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

/// One bound view query together with the fixed public column names captured when the view was created. `None` exists only while the catalog-opening migration reads formats written before column metadata was persisted.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredView {
    /// Stable logical relation identity. Renames and replacement preserve it; a zero value marks a legacy catalog row upgraded during initial open.
    #[serde(default)]
    pub(crate) object_id: [u8; 16],
    /// Durable SQL-role owner loaded from the typed view catalog row. The query-definition JSON deliberately excludes ownership so catalog definition and authorization state cannot disagree.
    #[serde(skip)]
    pub(crate) role_owner: String,
    /// Durable relation-wide ACL loaded from the typed view catalog row.
    #[serde(skip)]
    pub(crate) acl: Option<Vec<uqa_storage::TableAclEntry>>,
    /// Durable per-column ACLs loaded from the typed view catalog row.
    #[serde(skip)]
    pub(crate) column_acls: BTreeMap<String, Vec<uqa_storage::TableAclEntry>>,
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

impl StoredView {
    pub(crate) fn security(&self) -> TableSecurity {
        TableSecurity {
            role_owner: self.role_owner.clone(),
            acl: self.acl.clone(),
            column_acls: self.column_acls.clone(),
        }
    }

    pub(crate) fn set_security(&mut self, security: TableSecurity) {
        self.role_owner = security.role_owner;
        self.acl = security.acl;
        self.column_acls = security.column_acls;
    }

    pub(crate) fn security_invoker(&self) -> bool {
        self.options.iter().any(|(name, value)| {
            name == "security_invoker" && matches!(value.as_str(), "true" | "on" | "yes" | "1")
        })
    }
}

pub(super) struct DurableCatalogState {
    pub(super) domains: CatalogCell<BTreeMap<String, super::engine_domains::StoredDomain>>,
    pub(super) graphs: CatalogCell<BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>>,
    pub(super) models: CatalogCell<BTreeMap<String, DeepModel>>,
    pub(super) scoring_params: CatalogCell<BTreeMap<String, String>>,
    pub(super) views: CatalogCell<BTreeMap<RelationIdentity, StoredView>>,
    pub(super) catalog_indexes:
        CatalogCell<BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>>,
    pub(super) database_security: CatalogCell<DatabaseSecurity>,
    pub(super) schemas: CatalogCell<BTreeMap<String, SchemaSecurity>>,
    pub(super) path_indexes: CatalogCell<BTreeMap<String, uqa_graph::PathIndex>>,
    pub(super) sequences: CatalogCell<BTreeMap<RelationIdentity, SequenceState>>,
    pub(super) sequence_object_ids: CatalogCell<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub(super) sequence_persistence:
        CatalogCell<BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>>,
    pub(super) sequence_security: CatalogCell<BTreeMap<RelationIdentity, SequenceSecurity>>,
    pub(super) named_analyzers: CatalogCell<BTreeMap<String, String>>,
    pub(super) table_field_analyzers: CatalogCell<TableFieldAnalyzerRegistry>,
    pub(super) foreign_servers: CatalogCell<BTreeMap<String, uqa_fdw::ForeignServer>>,
    pub(super) foreign_tables:
        CatalogCell<BTreeMap<RelationIdentity, super::engine_fdw::StoredForeignTable>>,
    pub(super) foreign_table_security: CatalogCell<BTreeMap<RelationIdentity, TableSecurity>>,
    pub(super) sql_user_functions:
        CatalogCell<BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>>,
    pub(super) roles: CatalogCell<BTreeMap<String, super::engine_roles::RoleDefinition>>,
    pub(super) role_memberships: CatalogCell<
        BTreeMap<super::engine_roles::RoleMembershipKey, super::engine_roles::RoleMembership>,
    >,
    pub(super) triggers: CatalogCell<
        BTreeMap<
            uqa_storage::RelationIdentity,
            BTreeMap<String, super::engine_events::StoredTrigger>,
        >,
    >,
    pub(super) rules: CatalogCell<
        BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, super::engine_events::StoredRule>>,
    >,
}

#[derive(Clone)]
pub(super) struct DurableCatalogSnapshot {
    pub(super) domains: Arc<BTreeMap<String, super::engine_domains::StoredDomain>>,
    pub(super) graphs: Arc<BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>>,
    pub(super) models: Arc<BTreeMap<String, DeepModel>>,
    pub(super) scoring_params: Arc<BTreeMap<String, String>>,
    pub(super) views: Arc<BTreeMap<RelationIdentity, StoredView>>,
    pub(super) catalog_indexes: Arc<BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>>,
    pub(super) database_security: Arc<DatabaseSecurity>,
    pub(super) schemas: Arc<BTreeMap<String, SchemaSecurity>>,
    pub(super) path_indexes: Arc<BTreeMap<String, uqa_graph::PathIndex>>,
    pub(super) sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub(super) sequence_object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub(super) sequence_persistence:
        Arc<BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>>,
    pub(super) sequence_security: Arc<BTreeMap<RelationIdentity, SequenceSecurity>>,
    pub(super) named_analyzers: Arc<BTreeMap<String, String>>,
    pub(super) table_field_analyzers: Arc<TableFieldAnalyzerRegistry>,
    pub(super) foreign_servers: Arc<BTreeMap<String, uqa_fdw::ForeignServer>>,
    pub(super) foreign_tables:
        Arc<BTreeMap<RelationIdentity, super::engine_fdw::StoredForeignTable>>,
    pub(super) foreign_table_security: Arc<BTreeMap<RelationIdentity, TableSecurity>>,
    pub(super) sql_user_functions:
        Arc<BTreeMap<String, Vec<Arc<super::engine_user_functions::SQLUserFunction>>>>,
    pub(super) roles: Arc<BTreeMap<String, super::engine_roles::RoleDefinition>>,
    pub(super) role_memberships:
        Arc<BTreeMap<super::engine_roles::RoleMembershipKey, super::engine_roles::RoleMembership>>,
    pub(super) triggers: Arc<
        BTreeMap<
            uqa_storage::RelationIdentity,
            BTreeMap<String, super::engine_events::StoredTrigger>,
        >,
    >,
    pub(super) rules: Arc<
        BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, super::engine_events::StoredRule>>,
    >,
}

impl DurableCatalogState {
    pub(super) fn new() -> Self {
        Self {
            domains: CatalogCell::new(BTreeMap::new()),
            graphs: CatalogCell::new(BTreeMap::new()),
            models: CatalogCell::new(BTreeMap::new()),
            scoring_params: CatalogCell::new(BTreeMap::new()),
            views: CatalogCell::new(BTreeMap::new()),
            catalog_indexes: CatalogCell::new(BTreeMap::new()),
            database_security: CatalogCell::new(DatabaseSecurity::bootstrap()),
            schemas: CatalogCell::new(BTreeMap::from([(
                "public".to_string(),
                SchemaSecurity::legacy("public"),
            )])),
            path_indexes: CatalogCell::new(BTreeMap::new()),
            sequences: CatalogCell::new(BTreeMap::new()),
            sequence_object_ids: CatalogCell::new(BTreeMap::new()),
            sequence_persistence: CatalogCell::new(BTreeMap::new()),
            sequence_security: CatalogCell::new(BTreeMap::new()),
            named_analyzers: CatalogCell::new(BTreeMap::new()),
            table_field_analyzers: CatalogCell::new(BTreeMap::new()),
            foreign_servers: CatalogCell::new(BTreeMap::new()),
            foreign_tables: CatalogCell::new(BTreeMap::new()),
            foreign_table_security: CatalogCell::new(BTreeMap::new()),
            sql_user_functions: CatalogCell::new(BTreeMap::new()),
            roles: CatalogCell::new(BTreeMap::from([(
                "uqa".to_string(),
                super::engine_roles::RoleDefinition::bootstrap(),
            )])),
            role_memberships: CatalogCell::new(BTreeMap::new()),
            triggers: CatalogCell::new(BTreeMap::new()),
            rules: CatalogCell::new(BTreeMap::new()),
        }
    }

    /// Capture durable registries in the transaction coordinator's canonical lock order.
    pub(super) fn snapshot(&self) -> DurableCatalogSnapshot {
        DurableCatalogSnapshot {
            domains: self.domains.snapshot(),
            graphs: self.graphs.snapshot(),
            models: self.models.snapshot(),
            scoring_params: self.scoring_params.snapshot(),
            views: self.views.snapshot(),
            catalog_indexes: self.catalog_indexes.snapshot(),
            database_security: self.database_security.snapshot(),
            schemas: self.schemas.snapshot(),
            path_indexes: self.path_indexes.snapshot(),
            sequences: self.sequences.snapshot(),
            sequence_object_ids: self.sequence_object_ids.snapshot(),
            sequence_persistence: self.sequence_persistence.snapshot(),
            sequence_security: self.sequence_security.snapshot(),
            named_analyzers: self.named_analyzers.snapshot(),
            table_field_analyzers: self.table_field_analyzers.snapshot(),
            foreign_servers: self.foreign_servers.snapshot(),
            foreign_tables: self.foreign_tables.snapshot(),
            foreign_table_security: self.foreign_table_security.snapshot(),
            sql_user_functions: self.sql_user_functions.snapshot(),
            roles: self.roles.snapshot(),
            role_memberships: self.role_memberships.snapshot(),
            triggers: self.triggers.snapshot(),
            rules: self.rules.snapshot(),
        }
    }

    /// Restore shared immutable values in the transaction coordinator's lock order.
    pub(super) fn restore(&self, snapshot: &DurableCatalogSnapshot) {
        self.graphs.restore(&snapshot.graphs);
        self.models.restore(&snapshot.models);
        self.scoring_params.restore(&snapshot.scoring_params);
        self.views.restore(&snapshot.views);
        self.catalog_indexes.restore(&snapshot.catalog_indexes);
        self.database_security.restore(&snapshot.database_security);
        self.schemas.restore(&snapshot.schemas);
        self.path_indexes.restore(&snapshot.path_indexes);
        self.sequences.restore(&snapshot.sequences);
        self.sequence_object_ids
            .restore(&snapshot.sequence_object_ids);
        self.sequence_persistence
            .restore(&snapshot.sequence_persistence);
        self.sequence_security.restore(&snapshot.sequence_security);
        self.named_analyzers.restore(&snapshot.named_analyzers);
        self.table_field_analyzers
            .restore(&snapshot.table_field_analyzers);
        self.foreign_servers.restore(&snapshot.foreign_servers);
        self.foreign_tables.restore(&snapshot.foreign_tables);
        self.foreign_table_security
            .restore(&snapshot.foreign_table_security);
        self.sql_user_functions
            .restore(&snapshot.sql_user_functions);
        self.domains.restore(&snapshot.domains);
        self.roles.restore(&snapshot.roles);
        self.role_memberships.restore(&snapshot.role_memberships);
        self.triggers.restore(&snapshot.triggers);
        self.rules.restore(&snapshot.rules);
    }
}

pub(super) struct SessionContext {
    /// Positive process identifier exposed by `pg_backend_pid()` and asynchronous notification responses. Portal workers share the owning session context and therefore retain the same identifier.
    pub(super) backend_process_id: AtomicI32,
    pub(super) backend_process_id_is_local: AtomicBool,
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
    pub(crate) statistics_worker: AtomicBool,
    pub(crate) statistics_client: AtomicBool,
}

impl SessionContext {
    pub(super) fn new(random_state: super::SessionRandomState) -> Self {
        let state = super::SessionStateSnapshot {
            graph_overlay: None,
            search_path: vec!["public".to_string()],
            temporary_namespace_allocated: false,
            session_vars: BTreeMap::new(),
            sequence_currvals: BTreeMap::new(),
            last_sequence: None,
            prepared: BTreeMap::new(),
            sql_statement_cache: SQLStatementCache::default(),
            portal_names: BTreeSet::new(),
            listened_channels: Vec::new(),
            current_user: "uqa".to_string(),
            session_user: "uqa".to_string(),
        };
        Self {
            backend_process_id: AtomicI32::new(
                crate::engine_notifications::allocate_backend_process_id(),
            ),
            backend_process_id_is_local: AtomicBool::new(true),
            state: RwLock::new(state),
            sequence_caches: Mutex::new(BTreeMap::new()),
            random_state: Mutex::new(random_state),
            transactions: Mutex::new(Vec::new()),
            row_lock_statements: Mutex::new(Vec::new()),
            command_mutation_overlays: Mutex::new(Vec::new()),
            portals: Mutex::new(BTreeMap::new()),
            next_portal_id: Mutex::new(1),
            next_portal_transaction_origin: Mutex::new(1),
            statistics_worker: AtomicBool::new(false),
            statistics_client: AtomicBool::new(false),
        }
    }

    pub(super) fn install_database_backend_process_id(&self, process_id: i32) {
        let local_process_id = self.backend_process_id.swap(process_id, Ordering::AcqRel);
        let was_local = self
            .backend_process_id_is_local
            .swap(false, Ordering::AcqRel);
        assert!(
            was_local,
            "database backend process identifier installed twice"
        );
        crate::engine_notifications::release_backend_process_id(local_process_id);
    }
}

impl Drop for SessionContext {
    fn drop(&mut self) {
        if *self.backend_process_id_is_local.get_mut() {
            crate::engine_notifications::release_backend_process_id(
                *self.backend_process_id.get_mut(),
            );
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
    pub(super) notifications: Arc<Mutex<VecDeque<crate::SQLNotification>>>,
    pub(super) notification_wake: Arc<parking_lot::Condvar>,
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
            notifications: Arc::new(Mutex::new(VecDeque::new())),
            notification_wake: Arc::new(parking_lot::Condvar::new()),
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
    pub(super) storage_cache_revisions: Mutex<Option<uqa_storage::CatalogCacheRevisions>>,
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
            storage_cache_revisions: Mutex::new(None),
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
