//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level engine: a per-table [`DocumentStore`] + [`InvertedIndex`]
//! pair, document mutation entry points, and a minimal `search` API for
//! text-only round trips. Backed either by in-memory stores
//! ([`Engine::new`]), the `SQLite`/`SQLCipher` constructors, or a swappable
//! [`uqa_storage::PersistentStorageProvider`]; the operator pipeline is
//! identical across backends.
//!
//! # Public API surface
//!
//! Construction:
//! - [`Engine::new`] - purely in-memory; great for tests and the REPL.
//! - [`Engine::open`] - `SQLite`-backed catalog at the given path; reopens
//!   restore tables, models, and graphs from disk.
//! - [`Engine::open_encrypted`] - same catalog restore path, with a
//!   `SQLCipher` key applied before any schema access.
//! - [`Engine::open_compressed`] - schema-neutral compressed `SQLite` VFS.
//! - [`Engine::open_compressed_encrypted`] - compressed chunks encrypted
//!   after compression.
//! - [`Engine::from_persistent_provider`] - storage-neutral construction for
//!   redb and application-defined providers, with backend-neutral sessions.
//!
//! Schema and table lifecycle:
//! - [`Engine::create_table`] - register a table with declared columns.
//! - [`Engine::create_default_table`] - convenience for FTS-only tables.
//! - [`Engine::create_vector_field`] - attach a vector field to an
//!   existing table.
//!
//! Document mutation:
//! - [`Engine::add_document`], [`Engine::add_document_with_vectors`]
//! - [`Engine::add_vector`] - set or replace a vector for an existing doc.
//! - [`Engine::get_document`], [`Engine::delete_document`]
//! - [`Engine::document_count`]
//! - [`Engine::transaction`], [`Engine::sql_batch`] - group writes under one
//!   engine transaction.
//!
//! Querying:
//! - `Engine::sql` (defined in [`sql`]) - full SQL surface (select /
//!   insert / update / delete / create-table, plus the registered
//!   functions: `text_match`, `knn_match`, `fuse_bayesian_evidence` (plus
//!   exact alias `fuse_log_odds`), `pool_positive_evidence`,
//!   `multi_field_match`, `staged_retrieval`, `graph_*`, `deep_predict`).
//! - [`Engine::sql_cursor`] / [`Engine::sql_columnar`] - bounded, schema-ordered
//!   column batches for result sets that should not be retained in memory.
//! - [`Engine::search`] - direct text-only retrieval returning a posting
//!   list.
//! - [`Engine::knn_search`], [`Engine::vector_similarity_search`] - k-NN
//!   over a vector field.
//! - [`Engine::hybrid_search`] - exact signed single-prior fusion of text and
//!   vector posting lists (no SQL parsing in the hot path).
//! - [`Engine::robust_hybrid_search`] - explicitly requested gated,
//!   confidence-scaled positive-evidence pooling.
//!
//! Deep-model persistence:
//! - [`Engine::save_model`], [`Engine::load_model`], [`Engine::drop_model`]
//! - [`Engine::deep_predict`] - runs a stored model against the cached
//!   feature row and returns ranked `(doc_id, score)` pairs.
//!
//! Graph workspaces (used by the Cypher front-end and the `graph_*`
//! SQL functions):
//! - [`Engine::create_graph`], [`Engine::drop_graph`]
//! - [`Engine::graph_with`] - read-only access by name.
//! - [`Engine::graph_with_mut`] - exclusive mutable access.
//!
//! Result types ([`SQLResult`], [`SQLParam`]) are re-exported from
//! `uqa-sql`. Errors flow through [`EngineError`], which wraps SQL and
//! storage errors so callers only need to match one enum.

pub mod functions;
pub mod migration;
pub mod operator_tree_bridge;
pub mod sql;

mod async_sql_engine;
mod engine_analyzers;
mod engine_cancellation;
mod engine_capabilities;
mod engine_catalog_indexes;
mod engine_events;
mod engine_fdw;
mod engine_fts;
mod engine_generated;
mod engine_graphs;
mod engine_hierarchy;
mod engine_hook;
mod engine_models;
mod engine_open;
mod engine_relations;
mod engine_roles;
mod engine_search;
mod engine_sequences;
mod engine_session;
mod engine_sql_registry;
mod engine_state;
mod engine_statement_cache;
mod engine_table_storage;
mod engine_tables;
mod engine_transactions;
mod engine_truncate;
mod engine_user_functions;
mod row_locks;
mod value_index;

pub(crate) use sql::dml::{
    CommandExactIndex, CommandMutationOverlay, DeferredForeignKeyCheck, TransactionRowChange,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use uqa_analysis::{analyzer::standard_analyzer, registry as analyzer_registry, Analyzer};
use uqa_core::{DocId, FieldName, PostingEntry, PostingList, Value};
use uqa_ml::{
    deep_learn as ml_deep_learn, DeepLearnOutput, DeepModel, LearnOptions, TrainingExample,
    TrainingSet,
};
use uqa_operators::ExecutionContext;
use uqa_scoring::{
    BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics,
    CalibrationReport, ParameterLearner, RawBm25Score, Scorer, UnsupervisedBm25ScoreEstimator,
};
use uqa_sql::SQLError;
use uqa_storage::{
    document_store::Document, AnalyzerPhase, CatalogFacade, CatalogIndexRow, ColumnStatsInput,
    ColumnStatsRow, DocumentStore, EdgeRow, GraphSnapshot, GraphVertexRow, HNSWIndex,
    HNSWIndexParams, IVFIndex, IVFIndexParams, InvertedIndex, ManagedConnection,
    MemoryDocumentStore, MemoryInvertedIndex, MemoryVectorIndex, PersistentStorageBackend,
    PersistentStorageProvider, PersistentStorageSession, RelationIdentity,
    SQLiteCompressedContainerAnchor, SQLiteStorageProvider, SequenceRow, StorageBackendError,
    StorageBackendResult, StorageSavepointId, TableSchema, VectorFieldSchema, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec, ViewRow,
};

pub use sql::{SQLCursor, SQLCursorSummary};
pub use uqa_execution::{ColumnVector, ColumnarBatch};
pub use uqa_sql::{ast::SequenceRestart, AsyncSQLEngine, SQLParam, SQLResult};
pub use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions, SQLiteError};

use engine_state::{
    DurableCatalogSnapshot, DurableCatalogState, EpochCoordinator, QueryRuntime, RuntimeExtensions,
    SessionContext, StorageContext, StoredView, StoredViewKind,
};
use engine_statement_cache::{PreparedStatementPlan, SQLStatementCache};
use functions::RegisteredSQLFunction;
pub use functions::{
    SQLAggregateFunction, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility,
    SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult, SQLTableFunctionStream,
};

const SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";
/// Metadata key prefix for per-graph AGE label registries
/// (`graph_label_registry::<graph>` -> JSON `GraphLabelRegistry`).
const GRAPH_LABELS_METADATA_PREFIX: &str = "graph_label_registry::";
const FUNCTIONS_METADATA_KEY: &str = "sql_functions_json";
const ROLES_METADATA_KEY: &str = "sql_roles_json";
const ROLE_MEMBERSHIPS_METADATA_KEY: &str = "sql_role_memberships_json";
const TRIGGERS_METADATA_KEY: &str = "sql_triggers_json";
const RULES_METADATA_KEY: &str = "sql_rules_json";
/// Default nesting cap for user-defined function calls. Exceeding it
/// raises `stack depth limit exceeded`, mirroring the `PostgreSQL`
/// `max_stack_depth` guard.
const SQL_FUNCTION_DEPTH_LIMIT: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("SQL error: {0}")]
    SQL(#[from] SQLError),
    #[error("storage error: {0}")]
    Storage(#[from] SQLiteError),
}

pub type EngineResult<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone)]
pub struct ScoredEntry {
    pub doc_id: DocId,
    pub score: f64,
}

/// Algorithm that actually produced a text-search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSearchAlgorithm {
    Exhaustive,
    Wand,
    BlockMaxWand,
}

/// Observable work counters for one text top-k execution.
#[derive(Debug, Clone)]
pub struct TextSearchProfile {
    pub entries: Vec<ScoredEntry>,
    pub algorithm: TextSearchAlgorithm,
    pub scored_candidates: u64,
    /// Exact distinct candidates for exhaustive/materialized execution; for
    /// score-cursor WAND/BMW this is the sum of term document frequencies, a
    /// no-prescan upper bound on the distinct candidate count.
    pub total_candidates: u64,
    pub cursor_advances: u64,
    pub skip_rate: f64,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsIndexStat {
    pub table_name: String,
    pub field: String,
    pub analyzer: String,
    pub posting_count: u64,
    pub doc_length_count: u64,
    pub indexed_doc_count: u64,
    pub term_count: u64,
    pub total_field_length: u64,
}

impl ScoredEntry {
    fn from_entry(e: &PostingEntry) -> Self {
        Self {
            doc_id: e.doc_id,
            score: e.payload.score,
        }
    }
}

/// Scoring strategy passed to [`Engine::search`].
#[derive(Debug, Clone)]
pub enum ScoringMode {
    BM25(BM25Params),
    BayesianBM25(BayesianBM25Params),
}

impl Default for ScoringMode {
    fn default() -> Self {
        Self::BM25(BM25Params::default())
    }
}

type TableFieldAnalyzerRegistry = BTreeMap<(String, String), (String, String)>;
type SessionPortalTableSnapshots = Arc<BTreeMap<RelationIdentity, Arc<TableState>>>;
type SessionPortalViewSnapshots = Arc<BTreeMap<RelationIdentity, StoredView>>;
type SessionPortalSQLFunctionSnapshots =
    Arc<BTreeMap<String, Vec<Arc<engine_user_functions::SQLUserFunction>>>>;
type SessionPortalCatalogSnapshot = Arc<DurableCatalogSnapshot>;
type SessionPortalTransactionOverlay = Arc<BTreeMap<String, BTreeMap<DocId, Option<Document>>>>;
type ColumnStatsMap = BTreeMap<String, uqa_planner::ColumnStats>;
type TransactionRelationStates = BTreeMap<RelationIdentity, u64>;
type FixedTransactionCatalogBaseline = BTreeMap<[u8; 16], (RelationIdentity, Vec<u8>)>;
type NontransactionalColumnStats = Vec<NontransactionalColumnStatsEntry>;

#[derive(Clone)]
struct NontransactionalColumnStatsEntry {
    table_name: String,
    table_lifecycle_id: u64,
    stats: ColumnStatsMap,
    persistent: bool,
    autonomous: bool,
}

/// Unified query engine composed from explicit storage, durable-catalog,
/// session, extension, epoch, and query-runtime ownership domains.
pub struct Engine {
    storage: StorageContext,
    durable: Arc<DurableCatalogState>,
    session: Arc<SessionContext>,
    extensions: RuntimeExtensions,
    epochs: EpochCoordinator,
    runtime: QueryRuntime,
    row_locks: Arc<row_locks::RowLockManager>,
    session_id: u64,
    owns_session_registration: bool,
    query_table_snapshots: Option<SessionPortalTableSnapshots>,
    query_view_snapshots: Option<SessionPortalViewSnapshots>,
    query_sql_function_snapshots: Option<SessionPortalSQLFunctionSnapshots>,
    query_catalog_snapshot: Option<SessionPortalCatalogSnapshot>,
    query_transaction_overlay: Option<SessionPortalTransactionOverlay>,
    query_transaction_origin: Option<u64>,
}

/// Mutable state of a single SQL sequence.
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
pub struct SequenceState {
    pub start: i64,
    pub increment: i64,
    pub current: i64,
    /// Whether `current` has already been returned by `nextval`.  Keeping this
    /// bit avoids the lossy `start - increment` sentinel at BIGINT boundaries.
    #[serde(default = "sequence_state_called_default")]
    pub called: bool,
}

const fn sequence_state_called_default() -> bool {
    // Legacy serialized states used `current = start - increment`; treating
    // that value as called preserves their next allocation semantics.
    true
}

#[derive(Clone, Copy, Default)]
struct TransactionDirtyState {
    table_data: bool,
    table_catalog: bool,
    catalog_registry: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransactionIntent {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackendTransactionMode {
    Deferred,
    Writer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransactionStatus {
    Active,
    Failed,
    FailedBackendAborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TransactionCharacteristicsState {
    isolation: uqa_sql::ast::TransactionIsolationLevel,
    read_only: bool,
    deferrable: bool,
}

impl Default for TransactionCharacteristicsState {
    fn default() -> Self {
        Self {
            isolation: uqa_sql::ast::TransactionIsolationLevel::ReadCommitted,
            read_only: false,
            deferrable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConstraintIdentity {
    pub(crate) relation: RelationIdentity,
    pub(crate) name: String,
    pub(crate) object_id: Option<[u8; 16]>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConstraintModeState {
    all: Option<bool>,
    named: BTreeMap<ConstraintIdentity, bool>,
}

struct TransactionFrame {
    /// Whether this outer frame is an implicit SQL-driver boundary rather than a user-visible `BEGIN` block. A simple-query batch promotes it when the batch reaches `BEGIN`.
    implicit_statement: bool,
    storage_savepoint: Option<StorageSavepointId>,
    intent: TransactionIntent,
    backend_mode: BackendTransactionMode,
    status: TransactionStatus,
    characteristics: TransactionCharacteristicsState,
    first_snapshot_set: bool,
    fixed_snapshot: Option<FixedTransactionSnapshot>,
    /// Last committed table catalog installed while a fixed row snapshot is active. Immutable fingerprints distinguish this transaction's own DDL overlay from definitions refreshed from sibling commits.
    fixed_catalog_baseline: Option<FixedTransactionCatalogBaseline>,
    /// `PostgreSQL` transaction/subtransaction IDs along the active frame path. The first slot belongs to this frame and each following slot to the correspondingly indexed SQL savepoint.
    xid_levels: Vec<Option<u32>>,
    savepoints: Vec<TransactionSavepoint>,
    session_snapshot: SessionStateSnapshot,
    data_snapshot: Option<EngineDataSnapshot>,
    relation_states_at_begin: TransactionRelationStates,
    dirty_at_begin: TransactionDirtyState,
    /// Lock mark this frame started with. Rolling the whole frame back releases every acquisition at or above it, independent of the savepoint marks the frame allocated later.
    begin_lock_mark: u32,
    lock_mark: u32,
    next_lock_mark: u32,
    snapshot_change_baseline: row_locks::RowChangeBaseline,
    row_changes: Vec<TransactionRowChange>,
    deferred_foreign_key_checks: Vec<DeferredForeignKeyCheck>,
    deferred_constraint_trigger_events: Vec<sql::DeferredConstraintTriggerEvent>,
    constraint_modes: ConstraintModeState,
    /// Statistics written by ANALYZE are nontransactional in `PostgreSQL`. Keep the latest values outside savepoint snapshots so any rollback can restore them after transactional storage state is rolled back.
    nontransactional_column_stats: NontransactionalColumnStats,
}

enum FixedTransactionSnapshot {
    Pinned(Box<Engine>),
    Detached(SessionPortalTableSnapshots),
}

impl FixedTransactionSnapshot {
    fn table(&self, relation: &RelationIdentity) -> Option<Arc<TableState>> {
        match self {
            Self::Pinned(snapshot) => snapshot.storage.tables.read().get(relation).cloned(),
            Self::Detached(tables) => tables.get(relation).cloned(),
        }
    }

    fn table_for_live_relation(
        &self,
        relation: &RelationIdentity,
        live: &TableState,
    ) -> Option<Arc<TableState>> {
        let storage_generation = live.storage_generation();
        let exact = self.table(relation);
        if exact
            .as_ref()
            .is_some_and(|table| table.storage_generation() == storage_generation)
        {
            return exact;
        }
        match self {
            Self::Pinned(snapshot) => snapshot
                .storage
                .tables
                .read()
                .values()
                .find(|table| table.storage_generation() == storage_generation)
                .cloned(),
            Self::Detached(tables) => tables
                .values()
                .find(|table| table.storage_generation() == storage_generation)
                .cloned(),
        }
    }
}

struct TransactionSavepoint {
    name: String,
    storage_savepoint: StorageSavepointId,
    intent: TransactionIntent,
    characteristics: TransactionCharacteristicsState,
    session_snapshot: SessionStateSnapshot,
    data_snapshot: Option<EngineDataSnapshot>,
    relation_states_at_begin: TransactionRelationStates,
    dirty: TransactionDirtyState,
    lock_mark: u32,
    row_changes: Vec<TransactionRowChange>,
    deferred_foreign_key_checks: Vec<DeferredForeignKeyCheck>,
    deferred_constraint_trigger_events: Vec<sql::DeferredConstraintTriggerEvent>,
    constraint_modes: ConstraintModeState,
}

/// Lightweight SQL-session state that follows transaction/savepoint rollback
/// for every backend. It is intentionally separate from the database-sized
/// memory-engine snapshot so persistent sessions receive identical SET,
/// search-path, sequence-currval, PREPARE, and statement-cache semantics.
#[derive(Clone, Default)]
struct SessionStateSnapshot {
    search_path: Vec<String>,
    temporary_namespace_allocated: bool,
    session_vars: BTreeMap<String, String>,
    sequence_currvals: BTreeMap<RelationIdentity, i64>,
    prepared: BTreeMap<String, PreparedStatementPlan>,
    sql_statement_cache: SQLStatementCache,
    /// Names of portals that existed at this transaction or savepoint boundary. Rollback removes portals created later without rewinding cursor positions or resurrecting closed portals.
    portal_names: BTreeSet<String>,
    current_user: String,
    session_user: String,
}

struct SessionPortalState {
    data: SessionPortalData,
    columns: Vec<String>,
    column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    transaction_origin: u64,
    position: SessionPortalPosition,
    scrollable: bool,
    holdable: bool,
    /// The engine carries typed values rather than wire encodings; retaining the declaration format lets a `PostgreSQL` wire adapter request binary result encoding without changing portal execution.
    _binary: bool,
}

pub(crate) struct SessionPortalDeclaration {
    name: String,
    query: uqa_planner::QueryPlan,
    params: Vec<SQLParam>,
    columns: Vec<String>,
    column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    scrollable: bool,
    holdable: bool,
    binary: bool,
}

pub(crate) struct SessionPortalCommandDeclaration {
    name: String,
    command: Box<uqa_planner::CommandPlan>,
    params: Vec<SQLParam>,
    columns: Vec<String>,
    column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    scrollable: bool,
    /// `PostgreSQL` 18 materializes one `NULL`-filled tuple for each row produced by a modifying command opened with explicit `SCROLL`.
    null_returning_values: bool,
}

enum SessionPortalData {
    Pending {
        query: uqa_planner::QueryPlan,
        params: Vec<SQLParam>,
        table_snapshots: SessionPortalTableSnapshots,
        view_snapshots: SessionPortalViewSnapshots,
        sql_function_snapshots: SessionPortalSQLFunctionSnapshots,
        catalog_snapshot: SessionPortalCatalogSnapshot,
        restart: Option<SessionPortalRestart>,
    },
    PendingCommand {
        command: Box<uqa_planner::CommandPlan>,
        params: Vec<SQLParam>,
        /// Preserve `PostgreSQL` 18's explicit-`SCROLL` DML tuple image while retaining the command's returned row count.
        null_returning_values: bool,
    },
    Result(SQLResult),
    Indexed(SessionPortalMaterialization),
    Streaming {
        worker: SessionPortalWorker,
        materialized: Option<SessionPortalMaterialization>,
        eof: bool,
        restart: Option<SessionPortalRestart>,
    },
}

struct SessionPortalRestart {
    query: uqa_planner::QueryPlan,
    params: Vec<SQLParam>,
    table_snapshots: SessionPortalTableSnapshots,
    view_snapshots: SessionPortalViewSnapshots,
    sql_function_snapshots: SessionPortalSQLFunctionSnapshots,
    catalog_snapshot: SessionPortalCatalogSnapshot,
}

struct SessionPortalMaterialization {
    columns: Vec<String>,
    column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    rows: uqa_execution::IndexedSpill,
}

enum SessionPortalWorkerRequest {
    Step(uqa_execution::PhysicalScanDirection),
    Rewind,
    Close,
}

enum SessionPortalWorkerResponse {
    Started {
        columns: Vec<String>,
        column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    },
    Row(Vec<Value>),
    Eof,
    Rewound,
    Error(SQLError),
}

struct SessionPortalWorker {
    requests: std::sync::mpsc::Sender<SessionPortalWorkerRequest>,
    responses: std::sync::mpsc::Receiver<SessionPortalWorkerResponse>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SessionPortalWorker {
    fn drop(&mut self) {
        let _ = self.requests.send(SessionPortalWorkerRequest::Close);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// `PostgreSQL` distinguishes the positions before the first row and after the last row from a position on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPortalPosition {
    BeforeFirst,
    /// The one-based position of the current row.
    OnRow(usize),
    /// Number of rows processed while moving forward before the executor reported end of scan.
    AfterLast(usize),
}

#[derive(Clone, Copy)]
struct SessionRandomState {
    s0: u64,
    s1: u64,
}

impl Default for SessionRandomState {
    fn default() -> Self {
        Self {
            s0: 0x5851_f42d_4c95_7f2d,
            s1: 0x1405_7b7e_f767_814f,
        }
    }
}

#[derive(Clone)]
struct EngineDataSnapshot {
    tables: BTreeMap<RelationIdentity, TableDataSnapshot>,
    durable: DurableCatalogSnapshot,
    foreign_memory_tables: BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>,
}

#[derive(Clone)]
struct TableDataSnapshot {
    state: Arc<TableState>,
    storage_generation: [u8; 16],
    document_store: Arc<dyn DocumentStore>,
    inverted_index: Arc<dyn InvertedIndex>,
    vector_indexes: BTreeMap<FieldName, Arc<dyn VectorIndex>>,
    fts_fields: Vec<FieldName>,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    /// One-past-the-last allocated document id. `u128` is intentional: it
    /// represents `u64::MAX + 1`, so exhaustion is distinguishable from an
    /// available final id and can never wrap or issue a duplicate.
    next_id: u128,
    analyzer: Analyzer,
    column_stats: BTreeMap<String, uqa_planner::ColumnStats>,
    column_stats_loaded: bool,
    column_stats_dirty: bool,
    table_checks: Vec<uqa_sql::ast::TableCheck>,
    foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
    key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    hierarchy: uqa_sql::ast::TableHierarchy,
    doc_count_cache: u64,
    doc_count_dirty: bool,
}

pub(crate) struct TableState {
    /// Session-local relation generation. Reloads of the same catalog object preserve this value, while CREATE allocates a new value even when a dropped relation's name is reused.
    lifecycle_id: std::sync::atomic::AtomicU64,
    /// Durable logical relation identity used by `PostgreSQL` catalogs. Renames, schema changes, `TRUNCATE`, and reopen preserve it.
    object_id: [u8; 16],
    /// Durable physical-storage generation shared by every session. Schema-only changes preserve it; CREATE and TRUNCATE replace it so a fixed transaction snapshot never aliases a different physical relation lifetime.
    storage_generation: RwLock<[u8; 16]>,
    pub(crate) document_store: RwLock<Box<dyn DocumentStore>>,
    inverted_index: RwLock<Box<dyn InvertedIndex>>,
    vector_indexes: RwLock<BTreeMap<FieldName, Box<dyn VectorIndex>>>,
    fts_fields: RwLock<Vec<FieldName>>,
    /// Column schema captured at CREATE TABLE / ALTER TABLE time.
    /// Drives auto-id allocation and ALTER COLUMN bookkeeping.
    columns: RwLock<Vec<uqa_sql::ast::ColumnDef>>,
    /// Monotonic id watermark for SERIAL/BIGSERIAL columns. The first
    /// allocated value is `1`; the watermark grows past
    /// `max(existing_doc_id, allocated)` so reopened catalogs do not
    /// collide with existing rows.
    next_id: parking_lot::Mutex<u128>,
    analyzer: RwLock<Analyzer>,
    /// Per-column statistics refreshed by `ANALYZE table_name` or lazily
    /// by `column_stats` after writes mark the table dirty. Keyed by column
    /// name.
    column_stats: RwLock<BTreeMap<String, uqa_planner::ColumnStats>>,
    column_stats_loaded: AtomicBool,
    column_stats_dirty: AtomicBool,
    /// Table-level `CHECK` constraints, evaluated against every row
    /// at INSERT / UPDATE time.
    table_checks: RwLock<Vec<uqa_sql::ast::TableCheck>>,
    /// Table-level `FOREIGN KEY` constraints. Each entry binds local
    /// columns to a `(ref_table, ref_columns)` lookup target.
    foreign_keys: RwLock<Vec<uqa_sql::ast::ForeignKey>>,
    /// Typed PRIMARY KEY / UNIQUE tuples, including composite keys and
    /// their SQL NULL-equality policy.
    key_constraints: RwLock<Vec<uqa_sql::ast::TableKeyConstraint>>,
    /// Direct parents, an optional partition key, and an optional child bound.
    /// The complete object is persisted with the table's constraint envelope.
    hierarchy: RwLock<uqa_sql::ast::TableHierarchy>,
    /// Lazily built per-column value indexes for PRIMARY KEY / UNIQUE
    /// / `CREATE INDEX` btree columns. Maintained incrementally by the
    /// document write paths; cleared on bulk reloads.
    value_indexes: RwLock<BTreeMap<FieldName, value_index::ColumnValueIndex>>,
    /// Cached `document_store.len()`. Persistent stores answer `len`
    /// with a `COUNT(*)` query, which used to run once per SQL
    /// statement for planner row estimates; the cache is invalidated
    /// by every write and recomputed on demand.
    doc_count_cache: std::sync::atomic::AtomicU64,
    doc_count_dirty: AtomicBool,
    /// Immutable relation lifecycle attributes captured at creation.
    persistence: uqa_sql::ast::RelationPersistence,
    on_commit: uqa_sql::ast::OnCommitAction,
}

impl TableState {
    fn lifecycle_id(&self) -> u64 {
        self.lifecycle_id.load(Ordering::Acquire)
    }

    fn storage_generation(&self) -> [u8; 16] {
        *self.storage_generation.read()
    }

    fn object_id(&self) -> [u8; 16] {
        self.object_id
    }

    fn fts_fields(&self) -> Vec<FieldName> {
        self.fts_fields.read().clone()
    }
}

fn next_table_lifecycle_id() -> u64 {
    static NEXT_TABLE_LIFECYCLE_ID: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    NEXT_TABLE_LIFECYCLE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .expect("table lifecycle id space exhausted")
}

fn new_nonzero_table_identity(kind: &str) -> StorageBackendResult<[u8; 16]> {
    let mut identity = [0_u8; 16];
    getrandom::fill(&mut identity)
        .map_err(|error| StorageBackendError::Other(format!("allocate table {kind}: {error}")))?;
    if identity == [0; 16] {
        identity[15] = 1;
    }
    Ok(identity)
}

fn new_table_object_id() -> StorageBackendResult<[u8; 16]> {
    new_nonzero_table_identity("object identity")
}

fn new_table_storage_generation() -> StorageBackendResult<[u8; 16]> {
    new_nonzero_table_identity("storage generation")
}

fn normalize_analyzer_config_value(value: &mut serde_json::Value) {
    if let Some(tokenizer) = value.get_mut("tokenizer") {
        if let Some(name) = tokenizer.as_str() {
            *tokenizer = serde_json::json!({
                "type": name.to_ascii_lowercase().replace('-', "_")
            });
        }
    }
    if let Some(filters) = value
        .get_mut("token_filters")
        .and_then(|v| v.as_array_mut())
    {
        for filter in filters {
            if let Some(name) = filter.as_str() {
                *filter = serde_json::json!({
                    "type": name.to_ascii_lowercase().replace('-', "_")
                });
            }
        }
    }
}

fn parse_analyzer_config(name: &str, config_json: &str) -> std::result::Result<Analyzer, String> {
    let mut value: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| format!("analyzer `{name}` config is not valid JSON: {e}"))?;
    normalize_analyzer_config_value(&mut value);
    let analyzer: Analyzer = serde_json::from_value(value)
        .map_err(|e| format!("analyzer `{name}` config is not a valid analyzer: {e}"))?;
    analyzer
        .validate()
        .map_err(|e| format!("analyzer `{name}` config is invalid: {e}"))?;
    Ok(analyzer)
}

fn normalize_analyzer_phase(phase: &str) -> std::result::Result<(String, AnalyzerPhase), String> {
    let phase = AnalyzerPhase::parse(&phase.to_ascii_lowercase())?;
    let normalized = match phase {
        AnalyzerPhase::Index => "index",
        AnalyzerPhase::Search => "search",
        AnalyzerPhase::Both => "both",
    };
    Ok((normalized.to_string(), phase))
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.owns_session_registration {
            self.session.portals.lock().clear();
            if let Some(backend) = self.storage.backend.as_ref() {
                if backend.in_transaction() {
                    let _ = backend.rollback_transaction();
                }
            }
            self.row_locks.release_session(self.session_id);
        }
    }
}

impl Engine {
    /// In-memory engine. State lives only as long as this `Engine`.
    pub fn new() -> Self {
        let row_locks = Arc::new(row_locks::RowLockManager::new());
        let session_id = row_locks.allocate_session();
        Self {
            storage: StorageContext::memory(),
            durable: Arc::new(DurableCatalogState::new()),
            session: Arc::new(SessionContext::new(initial_random_state())),
            extensions: RuntimeExtensions::new(),
            epochs: EpochCoordinator::new(),
            runtime: QueryRuntime::new(SQL_FUNCTION_DEPTH_LIMIT),
            row_locks,
            session_id,
            owns_session_registration: true,
            query_table_snapshots: None,
            query_view_snapshots: None,
            query_sql_function_snapshots: None,
            query_catalog_snapshot: None,
            query_transaction_overlay: None,
            query_transaction_origin: None,
        }
    }
}

fn initial_random_state() -> SessionRandomState {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STATE: AtomicU64 = AtomicU64::new(0x4d59_5df4_d0f3_3173);
    random_state_from_seed(NEXT_STATE.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed))
}

fn random_state_from_seed(mut seed: u64) -> SessionRandomState {
    let mut splitmix64 = || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = seed;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    };
    let state = SessionRandomState {
        s0: splitmix64(),
        s1: splitmix64(),
    };
    if state.s0 == 0 && state.s1 == 0 {
        SessionRandomState::default()
    } else {
        state
    }
}

/// Exact signed single-prior hybrid-search arguments. Keeps
/// [`Engine::hybrid_search`] borrowing-friendly without an explosion of
/// positional parameters.
#[derive(Debug, Clone)]
pub struct HybridSearchParams<'a> {
    pub table: &'a str,
    pub text_field: &'a str,
    pub text_query: &'a str,
    pub vector_field: &'a str,
    pub query_vector: Vec<f32>,
    /// How many KNN candidates to pull from the vector index before
    /// fusion. Tune above `top_k` to widen the recall pool.
    pub knn_pool: usize,
    pub top_k: usize,
}

/// Explicit robust-ranking variant of [`HybridSearchParams`]. This contract
/// applies positive-evidence gating and confidence scaling rather than exact
/// single-prior Bayesian evidence fusion.
#[derive(Debug, Clone)]
pub struct RobustHybridSearchParams<'a> {
    pub table: &'a str,
    pub text_field: &'a str,
    pub text_query: &'a str,
    pub vector_field: &'a str,
    pub query_vector: Vec<f32>,
    /// How many KNN candidates to pull from the vector index before
    /// fusion. Tune above `top_k` to widen the recall pool.
    pub knn_pool: usize,
    /// Confidence-scaling exponent for robust positive-evidence pooling.
    /// Must be finite and in `[0, 1]`.
    pub alpha: f64,
    pub top_k: usize,
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
