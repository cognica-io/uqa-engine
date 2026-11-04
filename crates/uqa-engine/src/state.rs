//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ownership boundaries for storage, catalog, session, runtime, and epochs.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use uqa_sql::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_sql::catalog::security::schema_registry::SchemaRegistry;
use uqa_sql::catalog::{
    events::{RuleCatalog, StoredTrigger},
    type_definitions::{CapturedTypeDefinitions, TypeDefinitions},
};

use parking_lot::{Mutex, ReentrantMutex, RwLock};

mod catalog_capture;
mod catalog_cell;
mod generation_sources;
#[cfg(test)]
pub(crate) use generation_sources::SnapshotCut;
pub(crate) use generation_sources::SnapshotSources;
mod resource_capture;
mod table_capture;
mod table_membership;
pub(crate) use catalog_cell::{CatalogCell, CatalogSnapshot};
#[cfg(test)]
pub(crate) use table_membership::SnapshotTableCut;
pub(crate) use table_membership::{MembershipSnapshot, SnapshotTableSources, TableRegistry};
mod session_state_lock;
pub(crate) use session_state_lock::{message_level, SessionStateLock, SessionStateWriteGuard};

use super::{
    BayesianBM25Params, CommandMutationOverlay, RelationIdentity, SQLStatementCache, SequenceState,
    TableFieldAnalyzerRegistry, TransactionFrame,
};

pub(super) struct StorageContext {
    pub(super) tables: Arc<TableRegistry>,
    pub(super) memory_capture: Option<uqa_storage::memory_capture::MemoryCaptureRegistry>,
    pub(super) catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
    pub(super) backend: Option<Arc<dyn uqa_storage::PersistentStorageBackend>>,
    pub(super) provider: Option<Arc<dyn uqa_storage::PersistentStorageProvider>>,
    /// Autocommit reads used to reserve identities beyond a pinned snapshot.
    pub(super) document_identity_reader:
        Mutex<Option<Arc<dyn uqa_storage::PersistentStorageBackend>>>,
}

pub(crate) use uqa_execution::catalog::security::{
    BoundDatabaseSecurity, BoundSchemaSecurity, BoundSequenceSecurity, BoundTableSecurity,
};

impl StorageContext {
    pub(super) fn memory() -> Self {
        let registry = uqa_storage::memory_capture::MemoryCaptureRegistry::new();
        Self {
            tables: Arc::new(
                table_membership::TableRegistry::memory(&registry)
                    .expect("fresh memory capture domain"),
            ),
            memory_capture: Some(registry),
            catalog: None,
            backend: None,
            provider: None,
            document_identity_reader: Mutex::new(None),
        }
    }

    pub(super) fn persistent(
        catalog: Arc<dyn uqa_storage::CatalogFacade>,
        backend: Arc<dyn uqa_storage::PersistentStorageBackend>,
        provider: Option<Arc<dyn uqa_storage::PersistentStorageProvider>>,
    ) -> Self {
        Self {
            tables: Arc::new(table_membership::TableRegistry::persistent()),
            memory_capture: None,
            catalog: Some(catalog),
            backend: Some(backend),
            provider,
            document_identity_reader: Mutex::new(None),
        }
    }

    pub(super) fn shared_from(source: &Self) -> Self {
        Self {
            tables: Arc::clone(&source.tables),
            memory_capture: source.memory_capture.clone(),
            catalog: source.catalog.clone(),
            backend: source.backend.clone(),
            provider: source.provider.clone(),
            document_identity_reader: Mutex::new(None),
        }
    }

    pub(super) fn memory_cell<T: uqa_storage::memory_capture::MemoryCaptureSource>(
        &self,
        value: T,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> uqa_storage::StorageBackendResult<uqa_storage::memory_capture::MemoryCaptureCell<T>> {
        match self.table_capture_registry(persistence) {
            Some(registry) => registry.cell(value),
            None => Ok(uqa_storage::memory_capture::MemoryCaptureCell::new(value)),
        }
    }

    pub(super) fn table_capture_registry(
        &self,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Option<&uqa_storage::memory_capture::MemoryCaptureRegistry> {
        self.memory_capture
            .as_ref()
            .filter(|_| persistence != uqa_sql::ast::RelationPersistence::Temporary)
    }

    pub(super) fn memory_vector_indexes(
        &self,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> uqa_storage::StorageBackendResult<
        uqa_storage::memory_capture::MemoryCaptureCell<uqa_storage::vector_index::VectorIndexes>,
    > {
        let indexes = self.table_capture_registry(persistence).map_or_else(
            uqa_storage::vector_index::VectorIndexes::default,
            uqa_storage::vector_index::VectorIndexes::memory,
        );
        self.memory_cell(indexes, persistence)
    }

    pub(super) fn memory_value_indexes(
        &self,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> uqa_storage::StorageBackendResult<
        uqa_storage::memory_capture::MemoryCaptureCell<
            uqa_execution::catalog::index::value::ValueIndexes,
        >,
    > {
        self.memory_cell(
            uqa_execution::catalog::index::value::ValueIndexes::default(),
            persistence,
        )
    }
}

pub(crate) use uqa_execution::catalog::view::{StoredView, StoredViewKind};

pub(super) struct DurableCatalogState {
    pub(super) domains: CatalogCell<
        TypeDefinitions<super::domains::StoredDomain>,
        CapturedTypeDefinitions<super::domains::StoredDomain>,
    >,
    pub(super) enums: CatalogCell<
        uqa_execution::catalog::enum_type::EnumRegistry,
        CapturedTypeDefinitions<uqa_sql::catalog::enum_type::StoredEnum>,
    >,
    pub(super) ranges: CatalogCell<
        uqa_execution::catalog::range_type::RangeRegistry,
        CapturedTypeDefinitions<uqa_sql::catalog::range_type::StoredRange>,
    >,
    pub(super) shells: CatalogCell<
        uqa_execution::catalog::shell_type::ShellRegistry,
        CapturedTypeDefinitions<uqa_sql::catalog::shell_type::StoredShellType>,
    >,
    pub(super) comments: CatalogCell<
        uqa_execution::catalog::comments::CommentRegistry,
        Vec<(uqa_execution::catalog::comments::CommentAddress, String)>,
    >,
    pub(super) composites: CatalogCell<
        uqa_execution::catalog::composite_type::CompositeRegistry,
        CapturedTypeDefinitions<uqa_sql::catalog::composite_type::StoredComposite>,
    >,
    pub(super) graphs: CatalogCell<
        BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>,
        resource_capture::CapturedResources<uqa_graph::GraphImageSnapshot>,
    >,
    pub(super) models: CatalogCell<
        uqa_ml::model_catalog::ModelCatalog,
        uqa_ml::model_catalog::CapturedModelCatalog,
    >,
    pub(super) scoring_params: CatalogCell<
        uqa_storage::catalog::scoring_params::ScoringParameters,
        uqa_storage::catalog::scoring_params::CapturedScoringParameters,
    >,
    pub(super) views: CatalogCell<
        BTreeMap<RelationIdentity, StoredView>,
        uqa_sql::catalog::definition_capture::views::CapturedViews,
    >,
    pub(super) catalog_indexes: CatalogCell<
        BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>,
        Vec<(RelationIdentity, uqa_storage::CatalogIndexRow)>,
    >,
    pub(super) database_security: CatalogCell<BoundDatabaseSecurity>,
    pub(super) schemas: CatalogCell<
        SchemaRegistry,
        uqa_sql::catalog::security::schema_registry::CapturedSchemaRegistry,
    >,
    pub(super) path_indexes: CatalogCell<
        BTreeMap<String, uqa_graph::PathIndex>,
        resource_capture::CapturedResources<
            uqa_core::memory::Budgeted<uqa_graph::CapturedPathIndex>,
        >,
    >,
    pub(super) sequences: CatalogCell<
        BTreeMap<RelationIdentity, SequenceState>,
        Vec<(RelationIdentity, SequenceState)>,
    >,
    pub(super) sequence_object_ids: CatalogCell<
        BTreeMap<RelationIdentity, [u8; 16]>,
        uqa_sql::catalog::definition_capture::identities::CapturedSequenceObjects,
    >,
    /// The `pg_class` OIDs sequences recorded when they were created, by object identity.
    pub(super) sequence_catalog_oids: CatalogCell<
        BTreeMap<[u8; 16], u32>,
        uqa_sql::catalog::definition_capture::identities::CapturedSequenceOids,
    >,
    /// The OIDs graphs and their labels recorded when they were created, by graph name.
    pub(super) graph_catalog_oids: CatalogCell<
        uqa_sql::catalog::graph_oids::GraphOidRegistry,
        uqa_sql::catalog::graph_oids::CapturedGraphOidRegistry,
    >,
    pub(super) sequence_persistence: CatalogCell<
        BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>,
        Vec<(RelationIdentity, uqa_sql::ast::RelationPersistence)>,
    >,
    pub(super) sequence_security: CatalogCell<
        BTreeMap<RelationIdentity, BoundSequenceSecurity>,
        Vec<(RelationIdentity, BoundSequenceSecurity)>,
    >,
    pub(super) named_analyzers: CatalogCell<
        BTreeMap<String, Arc<uqa_analysis::CompiledAnalyzer>>,
        resource_capture::CapturedResources<uqa_storage::analyzer_image::AnalyzerImageSnapshot>,
    >,
    pub(super) table_field_analyzers: CatalogCell<
        TableFieldAnalyzerRegistry,
        uqa_storage::analyzer_binding::capture::CapturedFieldAnalyzerBindings,
    >,
    pub(super) foreign_wrappers: CatalogCell<
        uqa_sql::catalog::foreign_wrapper::ForeignWrappers,
        uqa_sql::catalog::foreign_wrapper::CapturedForeignWrappers,
    >,
    pub(super) foreign_servers: CatalogCell<
        uqa_sql::catalog::foreign_server::ForeignServers,
        uqa_sql::catalog::foreign_server::CapturedForeignServers,
    >,
    pub(super) foreign_tables: CatalogCell<
        BTreeMap<RelationIdentity, super::fdw::StoredForeignTable>,
        Vec<(
            RelationIdentity,
            uqa_execution::catalog::foreign::capture::CapturedForeignTable,
        )>,
    >,
    pub(super) foreign_table_security: CatalogCell<
        BTreeMap<RelationIdentity, BoundTableSecurity>,
        Vec<(
            RelationIdentity,
            uqa_sql::catalog::security::table_binding::CapturedTableSecurity,
        )>,
    >,
    pub(super) builtin_routine_security: CatalogCell<
        uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities,
        uqa_sql::catalog::security::builtin_routines::CapturedBuiltinRoutineSecurities,
    >,
    pub(super) system_relation_security: CatalogCell<
        uqa_sql::catalog::security::system_relations::SystemRelationSecurities,
        uqa_sql::catalog::security::system_relations::CapturedSystemRelationSecurities,
    >,
    pub(super) sql_user_functions: CatalogCell<
        BTreeMap<String, Vec<Arc<super::user_functions::SQLUserFunction>>>,
        uqa_sql::catalog::definition_capture::routines::CapturedRoutines,
    >,
    pub(super) roles:
        CatalogCell<RoleDefinitions, uqa_sql::catalog::roles::CapturedRoleDefinitions>,
    pub(super) role_memberships:
        CatalogCell<RoleMemberships, uqa_sql::catalog::roles::CapturedRoleMemberships>,
    pub(super) triggers: CatalogCell<
        BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, StoredTrigger>>,
        uqa_sql::catalog::definition_capture::events::CapturedTriggers,
    >,
    pub(super) rules:
        CatalogCell<RuleCatalog, uqa_sql::catalog::definition_capture::rules::CapturedRules>,
}

#[derive(Clone)]
pub(super) struct DurableCatalogSnapshot {
    pub(super) domains: Arc<TypeDefinitions<super::domains::StoredDomain>>,
    pub(super) enums: Arc<uqa_execution::catalog::enum_type::EnumRegistry>,
    pub(super) ranges: Arc<uqa_execution::catalog::range_type::RangeRegistry>,
    pub(super) shells: Arc<uqa_execution::catalog::shell_type::ShellRegistry>,
    pub(super) comments: Arc<uqa_execution::catalog::comments::CommentRegistry>,
    pub(super) composites: Arc<uqa_execution::catalog::composite_type::CompositeRegistry>,
    pub(super) graphs: Arc<BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>>>,
    pub(super) models: Arc<uqa_ml::model_catalog::ModelCatalog>,
    pub(super) scoring_params: Arc<uqa_storage::catalog::scoring_params::ScoringParameters>,
    pub(super) views: Arc<BTreeMap<RelationIdentity, StoredView>>,
    pub(super) catalog_indexes: Arc<BTreeMap<RelationIdentity, uqa_storage::CatalogIndexRow>>,
    pub(super) database_security: Arc<BoundDatabaseSecurity>,
    pub(super) schemas: Arc<SchemaRegistry>,
    pub(super) path_indexes: Arc<BTreeMap<String, uqa_graph::PathIndex>>,
    pub(super) sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub(super) sequence_object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub(super) sequence_catalog_oids: Arc<BTreeMap<[u8; 16], u32>>,
    pub(super) graph_catalog_oids: Arc<uqa_sql::catalog::graph_oids::GraphOidRegistry>,
    pub(super) sequence_persistence:
        Arc<BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>>,
    pub(super) sequence_security: Arc<BTreeMap<RelationIdentity, BoundSequenceSecurity>>,
    pub(super) named_analyzers: Arc<BTreeMap<String, Arc<uqa_analysis::CompiledAnalyzer>>>,
    pub(super) table_field_analyzers: Arc<TableFieldAnalyzerRegistry>,
    pub(super) foreign_wrappers: Arc<uqa_sql::catalog::foreign_wrapper::ForeignWrappers>,
    pub(super) foreign_servers: Arc<uqa_sql::catalog::foreign_server::ForeignServers>,
    pub(super) foreign_tables: Arc<BTreeMap<RelationIdentity, super::fdw::StoredForeignTable>>,
    pub(super) foreign_table_security: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
    pub(super) builtin_routine_security:
        Arc<uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities>,
    pub(super) system_relation_security:
        Arc<uqa_sql::catalog::security::system_relations::SystemRelationSecurities>,
    pub(super) sql_user_functions:
        Arc<BTreeMap<String, Vec<Arc<super::user_functions::SQLUserFunction>>>>,
    pub(super) roles: Arc<RoleDefinitions>,
    pub(super) role_memberships: Arc<RoleMemberships>,
    pub(super) triggers:
        Arc<BTreeMap<uqa_storage::RelationIdentity, BTreeMap<String, StoredTrigger>>>,
    pub(super) rules: Arc<RuleCatalog>,
}

impl DurableCatalogState {
    pub(super) fn new() -> Self {
        Self {
            domains: CatalogCell::new(TypeDefinitions::new()),
            enums: CatalogCell::new(TypeDefinitions::new()),
            ranges: CatalogCell::new(TypeDefinitions::new()),
            shells: CatalogCell::new(TypeDefinitions::new()),
            comments: CatalogCell::new(BTreeMap::new()),
            composites: CatalogCell::new(TypeDefinitions::new()),
            graphs: CatalogCell::new(BTreeMap::new()),
            models: CatalogCell::new(uqa_ml::model_catalog::ModelCatalog::default()),
            scoring_params: CatalogCell::new(
                uqa_storage::catalog::scoring_params::ScoringParameters::default(),
            ),
            views: CatalogCell::new(BTreeMap::new()),
            catalog_indexes: CatalogCell::new(BTreeMap::new()),
            database_security: CatalogCell::new(BoundDatabaseSecurity::bootstrap()),
            schemas: CatalogCell::new(BoundSchemaSecurity::initial_catalog()),
            path_indexes: CatalogCell::new(BTreeMap::new()),
            sequences: CatalogCell::new(BTreeMap::new()),
            sequence_object_ids: CatalogCell::new(BTreeMap::new()),
            sequence_catalog_oids: CatalogCell::new(BTreeMap::new()),
            graph_catalog_oids: CatalogCell::new(
                uqa_sql::catalog::graph_oids::GraphOidRegistry::new(),
            ),
            sequence_persistence: CatalogCell::new(BTreeMap::new()),
            sequence_security: CatalogCell::new(BTreeMap::new()),
            named_analyzers: CatalogCell::new(BTreeMap::new()),
            table_field_analyzers: CatalogCell::new(BTreeMap::new()),
            foreign_wrappers: CatalogCell::new(uqa_sql::catalog::foreign_wrapper::native_wrappers()),
            foreign_servers: CatalogCell::new(
                uqa_sql::catalog::foreign_server::ForeignServers::new(),
            ),
            foreign_tables: CatalogCell::new(BTreeMap::new()),
            foreign_table_security: CatalogCell::new(BTreeMap::new()),
            builtin_routine_security: CatalogCell::new(
                uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities::new(),
            ),
            system_relation_security: CatalogCell::new(
                uqa_sql::catalog::security::system_relations::SystemRelationSecurities::new(),
            ),
            sql_user_functions: CatalogCell::new(BTreeMap::new()),
            roles: CatalogCell::new(RoleDefinitions::initial_catalog()),
            role_memberships: CatalogCell::new(RoleMemberships::new()),
            triggers: CatalogCell::new(BTreeMap::new()),
            rules: CatalogCell::new(BTreeMap::new()),
        }
    }

    /// Capture durable registries in the transaction coordinator's canonical lock order.
    pub(super) fn snapshot(&self) -> DurableCatalogSnapshot {
        DurableCatalogSnapshot {
            domains: self.domains.snapshot(),
            enums: self.enums.snapshot(),
            ranges: self.ranges.snapshot(),
            shells: self.shells.snapshot(),
            comments: self.comments.snapshot(),
            composites: self.composites.snapshot(),
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
            sequence_catalog_oids: self.sequence_catalog_oids.snapshot(),
            graph_catalog_oids: self.graph_catalog_oids.snapshot(),
            sequence_persistence: self.sequence_persistence.snapshot(),
            sequence_security: self.sequence_security.snapshot(),
            named_analyzers: self.named_analyzers.snapshot(),
            table_field_analyzers: self.table_field_analyzers.snapshot(),
            foreign_wrappers: self.foreign_wrappers.snapshot(),
            foreign_servers: self.foreign_servers.snapshot(),
            foreign_tables: self.foreign_tables.snapshot(),
            foreign_table_security: self.foreign_table_security.snapshot(),
            builtin_routine_security: self.builtin_routine_security.snapshot(),
            system_relation_security: self.system_relation_security.snapshot(),
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
        self.sequence_catalog_oids
            .restore(&snapshot.sequence_catalog_oids);
        self.graph_catalog_oids
            .restore(&snapshot.graph_catalog_oids);
        self.sequence_persistence
            .restore(&snapshot.sequence_persistence);
        self.sequence_security.restore(&snapshot.sequence_security);
        self.named_analyzers.restore(&snapshot.named_analyzers);
        self.table_field_analyzers
            .restore(&snapshot.table_field_analyzers);
        self.foreign_wrappers.restore(&snapshot.foreign_wrappers);
        self.foreign_servers.restore(&snapshot.foreign_servers);
        self.foreign_tables.restore(&snapshot.foreign_tables);
        self.foreign_table_security
            .restore(&snapshot.foreign_table_security);
        self.builtin_routine_security
            .restore(&snapshot.builtin_routine_security);
        self.system_relation_security
            .restore(&snapshot.system_relation_security);
        self.sql_user_functions
            .restore(&snapshot.sql_user_functions);
        self.enums.restore(&snapshot.enums);
        self.ranges.restore(&snapshot.ranges);
        self.shells.restore(&snapshot.shells);
        self.comments.restore(&snapshot.comments);
        self.composites.restore(&snapshot.composites);
        self.domains.restore(&snapshot.domains);
        self.roles.restore(&snapshot.roles);
        self.role_memberships.restore(&snapshot.role_memberships);
        self.triggers.restore(&snapshot.triggers);
        self.rules.restore(&snapshot.rules);
    }
}

pub(super) struct SessionContext {
    /// Start of the outer SQL message, shared with nested execution and portal workers.
    pub(super) statement_started_at_micros: AtomicI64,
    /// Positive process identifier exposed by `pg_backend_pid()` and asynchronous notification responses. Portal workers share the owning session context and therefore retain the same identifier.
    pub(super) backend_process_id: AtomicI32,
    pub(super) backend_process_id_is_local: AtomicBool,
    /// Host transport policy is monotonic and nontransactional; rollback and DISCARD cannot restore SQL listener capability.
    pub(super) notification_subscriptions_required: AtomicBool,
    /// Transactional session values share one lock so snapshots and restores
    /// cannot observe a mixture of old and new search-path, sequence,
    /// or statement-cache state.
    pub(super) state: SessionStateLock,
    /// Configuration state that transactions do not restore: custom parameter placeholders, loaded libraries and the values the client set at startup.
    pub(super) parameters: Mutex<crate::session::SessionParameterRegistry>,
    /// The bytes a query workspace may hold before it spills when the host bounds it below `work_mem`; 0 leaves `work_mem` in effect.
    pub(super) query_memory_limit: std::sync::atomic::AtomicUsize,
    /// Explicit close, owner Drop and automatic termination share one cleanup owner and its retained completion responsibility.
    pub(super) closure: crate::transactions::SessionClosure,
    /// Prepared definitions belong to the connection and survive transaction or
    /// savepoint rollback, including definitions created or removed after a boundary.
    pub(super) prepared: RwLock<
        uqa_core::memory::OwnedMap<
            uqa_core::memory::Produced<String>,
            super::PreparedStatementPlan,
        >,
    >,
    pub(super) next_prepared_handle: std::sync::atomic::AtomicU64,
    /// `PostgreSQL` sequence reservations are session-local and nontransactional. They are intentionally kept outside `SessionStateSnapshot` so rollback never rewinds consumption or restores blocks discarded by `ALTER SEQUENCE`.
    pub(super) sequence_caches:
        Mutex<BTreeMap<super::RelationIdentity, super::SessionSequenceCache>>,
    /// The sequence catalog as this session last read it from the latest commit, kept while nothing it was read from has changed.
    pub(super) sequence_snapshot: Mutex<Option<crate::sequence_snapshot::SequenceSnapshotMemo>>,
    pub(super) enum_type_comparison_states: uqa_sql::expr::enums::EnumTypeComparisonStates,
    pub(super) range_comparison_states:
        uqa_execution::catalog::range_type::support::RangeComparisonStates,
    /// How many times this session read the sequence catalog instead of reusing its last read.
    #[cfg(test)]
    pub(super) sequence_snapshot_reads: std::sync::atomic::AtomicU64,
    /// `PostgreSQL`'s session PRNG is not transactional: failed statements and
    /// transaction or savepoint rollback leave every consumed draw in place.
    pub(super) random_state: Mutex<super::SessionRandomState>,
    pub(super) transactions: Mutex<Vec<TransactionFrame>>,
    pub(super) completion_counters: Mutex<crate::transactions::CompletionCounters>,
    /// Memory and serialized providers retain query selections under one session allowance, shared by nested workers and surviving views.
    pub(super) query_retention: uqa_core::memory::MemoryBudget,
    pub(super) regex_cache: Mutex<Option<Arc<uqa_sql::expr::RegexCache>>>,
    pub(super) diskann_temporary: uqa_storage::diskann_index::build::DiskANNTemporaryBudget,
    /// One row-lock recheck context per in-flight SQL statement. Query-bearing commands, prepared execution, and `EXPLAIN ANALYZE` spawn nested plan executors that must share the outermost statement's context, while a host-callback statement nested inside another statement owns its own frame.
    pub(super) row_lock_statements: Mutex<
        Vec<Option<std::sync::Arc<uqa_execution::row_locks::retry_cache::RowLockRetryCache>>>,
    >,
    pub(super) command_mutation_overlays: Mutex<Vec<CommandMutationOverlay>>,
    pub(super) portals: Mutex<BTreeMap<String, super::SessionPortalState>>,
    pub(super) portal_registry: uqa_execution::statement::portal::PortalRegistry,
    pub(super) next_portal_transaction_origin: Mutex<u64>,
    pub(crate) statistics_worker: AtomicBool,
    pub(crate) statistics_client: AtomicBool,
    /// Enum labels that the outermost transaction added to types it did not create; they become usable at commit. Label checks run while catalog restoration holds the transaction stack, so this state has its own lock.
    pub(crate) uncommitted_enum_labels: Mutex<uqa_execution::schema::enums::UncommittedEnumLabels>,
    /// Routine source bodies this session compiled, as `PostgreSQL`'s backend-local function cache keeps them. The cache is not transactional, and portal workers share it with their session.
    pub(crate) routine_bodies: uqa_execution::routines::invocation::bodies::SessionRoutineBodies,
    /// Row changes of this session's commits that no maintenance record counts yet.
    pub(crate) kept_statistics: Mutex<crate::statistics::StatisticsChanges>,
}

#[derive(Clone)]
pub(super) enum RuntimeParameterValue {
    Setting(Option<String>),
    SearchPath {
        setting: Option<String>,
        path: Vec<String>,
    },
    Role(Option<Arc<uqa_sql::catalog::roles::identity::RoleBinding>>),
    SessionAuthorization(Arc<uqa_sql::catalog::roles::identity::RoleBinding>),
}

impl SessionContext {
    pub(super) fn new(random_state: super::SessionRandomState) -> Self {
        Self::with_retention(
            random_state,
            uqa_core::memory::MemoryBudget::new(
                uqa_storage::mvcc::VersionedSessionOptions::default().retained_bytes,
            ),
        )
    }

    pub(super) fn with_retention(
        random_state: super::SessionRandomState,
        query_retention: uqa_core::memory::MemoryBudget,
    ) -> Self {
        let state = super::SessionStateSnapshot {
            command_overlays: Vec::new(),
            graph_overlay: None,
            search_path: crate::session::default_search_path(),
            temporary_namespace: None,
            session_vars: BTreeMap::new(),
            parameter_scopes: uqa_sql::semantics::parameters::ParameterScopes::default(),
            sequence_currvals: BTreeMap::new(),
            last_sequence: None,
            sequence_discard_generation: 0,
            sql_statement_cache: SQLStatementCache::default(),
            portal_names: BTreeSet::new(),
            listened_channels: Vec::new(),
            authorization: uqa_sql::catalog::roles::session::SessionAuthorization::default(),
        };
        Self {
            statement_started_at_micros: AtomicI64::new(0),
            backend_process_id: AtomicI32::new(crate::notifications::allocate_backend_process_id()),
            backend_process_id_is_local: AtomicBool::new(true),
            notification_subscriptions_required: AtomicBool::new(false),
            state: SessionStateLock::new(state),
            parameters: Mutex::new(crate::session::SessionParameterRegistry::default()),
            query_memory_limit: std::sync::atomic::AtomicUsize::new(0),
            closure: crate::transactions::SessionClosure::default(),
            prepared: RwLock::new(uqa_core::memory::OwnedMap::new()),
            next_prepared_handle: std::sync::atomic::AtomicU64::new(0),
            sequence_caches: Mutex::new(BTreeMap::new()),
            sequence_snapshot: Mutex::new(None),
            enum_type_comparison_states: uqa_sql::expr::enums::EnumTypeComparisonStates::default(),
            range_comparison_states:
                uqa_execution::catalog::range_type::support::RangeComparisonStates::default(),
            #[cfg(test)]
            sequence_snapshot_reads: std::sync::atomic::AtomicU64::new(0),
            random_state: Mutex::new(random_state),
            transactions: Mutex::new(Vec::new()),
            completion_counters: Mutex::new(crate::transactions::CompletionCounters::default()),
            regex_cache: Mutex::new(None),
            query_retention,
            diskann_temporary: uqa_storage::diskann_index::build::DiskANNTemporaryBudget::new(
                uqa_storage::diskann_index::DiskANNIndexOptions::DEFAULT_TEMPORARY_BYTES,
            ),
            row_lock_statements: Mutex::new(Vec::new()),
            command_mutation_overlays: Mutex::new(Vec::new()),
            portals: Mutex::new(BTreeMap::new()),
            portal_registry: uqa_execution::statement::portal::PortalRegistry::default(),
            next_portal_transaction_origin: Mutex::new(1),
            statistics_worker: AtomicBool::new(false),
            statistics_client: AtomicBool::new(false),
            uncommitted_enum_labels: Mutex::new(
                uqa_execution::schema::enums::UncommittedEnumLabels::default(),
            ),
            routine_bodies:
                uqa_execution::routines::invocation::bodies::SessionRoutineBodies::default(),
            kept_statistics: Mutex::new(BTreeMap::new()),
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
        crate::notifications::release_backend_process_id(local_process_id);
    }
}

impl Drop for SessionContext {
    fn drop(&mut self) {
        if *self.backend_process_id_is_local.get_mut() {
            crate::notifications::release_backend_process_id(*self.backend_process_id.get_mut());
        }
    }
}

pub(super) struct RuntimeExtensions {
    pub(super) parallel: uqa_execution::parallel::ParallelExecutor,
    pub(super) foreign_memory_tables:
        Arc<RwLock<BTreeMap<RelationIdentity, Vec<uqa_sql::ResultRow>>>>,
    pub(super) functions: Arc<crate::sql_registry::Registry>,
}

impl RuntimeExtensions {
    pub(super) fn new(session: &Arc<SessionContext>, gate: &Arc<StatementGate>) -> Self {
        Self {
            parallel: uqa_execution::parallel::ParallelExecutor::default(),
            foreign_memory_tables: Arc::new(RwLock::new(BTreeMap::new())),
            functions: Arc::new(crate::sql_registry::Registry::new(session, gate)),
        }
    }

    pub(super) fn shared_from(source: &Self) -> Self {
        Self {
            parallel: source.parallel.clone(),
            foreign_memory_tables: source.foreign_memory_tables.clone(),
            functions: Arc::clone(&source.functions),
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

    /// Hold the gate only if no other thread holds it.
    pub(super) fn try_lock(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        self.mutex.try_lock()
    }

    /// Registry changes require an idle session, including on the calling thread.
    pub(super) fn try_lock_idle(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        if self.mutex.is_owned_by_current_thread() {
            None
        } else {
            self.mutex.try_lock()
        }
    }

    #[cfg(test)]
    pub(super) fn owns_current_operation(&self) -> bool {
        let identity = std::ptr::from_ref(self) as usize;
        self.mutex.is_owned_by_current_thread()
            || DELEGATED_STATEMENT_GATES.with(|gates| gates.borrow().contains(&identity))
    }

    pub(super) fn lock_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<Option<parking_lot::ReentrantMutexGuard<'_, ()>>, uqa_core::QueryCancelled> {
        cancellation.check()?;
        let identity = std::ptr::from_ref(self) as usize;
        if DELEGATED_STATEMENT_GATES.with(|delegated| delegated.borrow().contains(&identity)) {
            return Ok(None);
        }
        loop {
            cancellation.check()?;
            if let Some(guard) = self
                .mutex
                .try_lock_for(std::time::Duration::from_millis(10))
            {
                cancellation.check()?;
                return Ok(Some(guard));
            }
            #[cfg(test)]
            crate::notifications::gate_waited();
        }
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
    /// Installation and reads share one cell with internal readers and portal workers.
    pub(super) function_context: Arc<std::sync::OnceLock<super::SQLFunctionContext>>,
    pub(super) diagnostics: uqa_execution::query::diagnostics::QueryDiagnostics,
    pub(super) statement_gate: Arc<StatementGate>,
    pub(super) sql_execution_depth: AtomicUsize,
    pub(super) cancellation: uqa_core::CancellationToken,
    pub(super) notices: Arc<uqa_execution::query::NoticeQueue>,
    pub(super) notifications: Arc<Mutex<VecDeque<crate::SQLNotification>>>,
    pub(super) bounded_sql_notifications:
        Mutex<Option<Arc<crate::notifications::SQLNotificationInbox>>>,
    /// The terminations the session has scheduled for its idle period and transaction.
    pub(super) terminations: crate::session::SessionTerminations,
    pub(super) notification_wake: Arc<parking_lot::Condvar>,
    pub(super) function_depth_limit: AtomicUsize,
    pub(super) bayesian_params_cache: RwLock<BTreeMap<String, BayesianBM25Params>>,
    pub(super) regtype_output_cache: uqa_execution::catalog::cache::RegtypeOutputCache,
    pub(super) enum_label_cache: uqa_execution::catalog::enum_type::EnumLabelCache,
    pub(super) composite_descriptor_cache:
        uqa_execution::catalog::composite_type::CompositeDescriptorCache,
    pub(super) physical_index_cache: uqa_execution::catalog::index::physical::PhysicalIndexCache,
    pub(super) enforced_key_cache: uqa_execution::catalog::index::EnforcedKeyCache,
}

impl QueryRuntime {
    /// The runtime of a session whose notices reach the client at `client_level` (`client_min_messages`).
    pub(super) fn new(
        function_depth_limit: usize,
        client_level: Arc<std::sync::atomic::AtomicU8>,
        io_timing: Arc<AtomicBool>,
    ) -> Self {
        Self::with_cancellation(
            function_depth_limit,
            uqa_core::CancellationToken::new(),
            client_level,
            io_timing,
        )
    }

    pub(super) fn with_cancellation(
        function_depth_limit: usize,
        cancellation: uqa_core::CancellationToken,
        client_level: Arc<std::sync::atomic::AtomicU8>,
        io_timing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            function_context: Arc::new(std::sync::OnceLock::new()),
            diagnostics: uqa_execution::query::diagnostics::QueryDiagnostics::new(io_timing),
            statement_gate: Arc::new(StatementGate::new()),
            sql_execution_depth: AtomicUsize::new(0),
            cancellation,
            notices: Arc::new(uqa_execution::query::NoticeQueue::new(client_level)),
            notifications: Arc::new(Mutex::new(VecDeque::new())),
            bounded_sql_notifications: Mutex::new(None),
            terminations: crate::session::SessionTerminations::new(true),
            notification_wake: Arc::new(parking_lot::Condvar::new()),
            function_depth_limit: AtomicUsize::new(function_depth_limit),
            bayesian_params_cache: RwLock::new(BTreeMap::new()),
            regtype_output_cache: uqa_execution::catalog::cache::RegtypeOutputCache::default(),
            enum_label_cache: uqa_execution::catalog::enum_type::EnumLabelCache::default(),
            composite_descriptor_cache:
                uqa_execution::catalog::composite_type::CompositeDescriptorCache::default(),
            physical_index_cache:
                uqa_execution::catalog::index::physical::PhysicalIndexCache::default(),
            enforced_key_cache: uqa_execution::catalog::index::EnforcedKeyCache::default(),
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
    pub(super) seen_storage_read_view: Mutex<Option<uqa_storage::key_value::KeyValueReadRevision>>,
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
            seen_storage_read_view: Mutex::new(None),
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
mod memory_capture_tests;
#[cfg(test)]
mod tests;
