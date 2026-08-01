//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level engine: a per-table [`DocumentStore`] + [`InvertedIndex`]
//! pair, document mutation entry points, and a minimal `search` API for
//! text-only round trips. Backed either by in-memory stores
//! ([`Engine::new`]) or by `SQLite`, `SQLCipher`, and compressed `SQLite`
//! containers ([`Engine::open`], [`Engine::open_encrypted`],
//! [`Engine::open_compressed`], [`Engine::open_compressed_encrypted`]);
//! the operator pipeline is identical across backends.
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
//!   functions: `text_match`, `knn_match`, `fuse_bayesian_evidence`,
//!   `pool_positive_evidence` (plus compatibility alias `fuse_log_odds`),
//!   `multi_field_match`, `staged_retrieval`, `graph_*`, `deep_predict`).
//! - [`Engine::search`] - direct text-only retrieval returning a posting
//!   list.
//! - [`Engine::knn_search`], [`Engine::vector_similarity_search`] - k-NN
//!   over a vector field.
//! - [`Engine::hybrid_search`] - robust positive-evidence pooling of text
//!   and vector posting lists (no SQL parsing in the hot path).
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

mod engine_analyzers;
mod engine_cancellation;
mod engine_catalog_indexes;
mod engine_fdw;
mod engine_fts;
mod engine_graphs;
mod engine_models;
mod engine_open;
mod engine_relations;
mod engine_search;
mod engine_sequences;
mod engine_session;
mod engine_sql_registry;
mod engine_table_storage;
mod engine_tables;
mod engine_transactions;
mod engine_truncate;
mod engine_user_functions;
mod value_index;

use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
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
    document_store::Document, AnalyzerPhase, Catalog, CatalogFacade, CatalogIndexRow,
    ColumnStatsInput, ColumnStatsRow, DocumentStore, EdgeRow, GraphSnapshot, GraphVertexRow,
    IVFIndex, InvertedIndex, ManagedConnection, MemoryDocumentStore, MemoryInvertedIndex,
    MemoryVectorIndex, PersistentStorageBackend, PersistentVectorIndexParams, RelationIdentity,
    SQLiteStorageBackend, SequenceRow, StorageBackendError, StorageBackendResult, TableSchema,
    VectorFieldSchema, VectorIndex, ViewRow,
};

pub use uqa_sql::{ast::SequenceRestart, SQLParam, SQLResult};
pub use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions, SQLiteError};

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
const SQL_STATEMENT_CACHE_LIMIT: usize = 256;
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

/// Engine: per-table document store + inverted index + vector indexes,
/// each behind a `RwLock<Box<dyn ...>>` so the `Memory*` and `SQLite*`
/// backends drop in interchangeably.
pub struct Engine {
    /// Re-entrant statement boundary for one logical SQL session. `Engine`
    /// methods may call back into compiled SQL routines on the same thread,
    /// while concurrent callers must never interleave physical work on the
    /// session's pinned transaction connection or session-local registries.
    statement_gate: parking_lot::ReentrantMutex<()>,
    /// Session-bound table stores. Logical table definitions are durable and
    /// synchronized through `table_catalog_epoch`, but every entry here is
    /// built from this engine session's backend so explicit transactions never
    /// leak onto another session's `SQLite` connection.
    tables: RwLock<BTreeMap<RelationIdentity, Arc<TableState>>>,
    catalog: Option<Arc<dyn CatalogFacade>>,
    backend: Option<Arc<dyn PersistentStorageBackend>>,
    /// `SQLite` handle used to derive independent logical sessions. Catalog and
    /// backend objects for this `Engine` are always built from clones of this
    /// exact handle, preserving cross-store transaction affinity.
    sqlite_session: Option<ManagedConnection>,
    /// Last `PRAGMA data_version` observed on this session's dedicated monitor
    /// connection. Unlike in-process epochs, this detects commits made by an
    /// independently opened `Engine` or another process.
    seen_sqlite_data_version: std::sync::atomic::AtomicU64,
    external_commit_refresh: parking_lot::Mutex<()>,
    /// Shared logical-catalog generation for sessions derived from the same
    /// engine. Physical table stores remain session-bound and are rebound when
    /// this generation advances.
    table_catalog_epoch: Arc<std::sync::atomic::AtomicU64>,
    seen_table_catalog_epoch: std::sync::atomic::AtomicU64,
    table_catalog_dirty: AtomicBool,
    table_catalog_refresh: parking_lot::Mutex<()>,
    /// Generation for committed table contents shared by sessions derived
    /// from this engine. Table stores and their value/vector/text caches are
    /// session-bound, so a sibling commit invalidates every derived cache
    /// before the next statement starts.
    table_data_epoch: Arc<std::sync::atomic::AtomicU64>,
    seen_table_data_epoch: std::sync::atomic::AtomicU64,
    table_data_dirty: AtomicBool,
    table_data_refresh: parking_lot::Mutex<()>,
    /// Generation for durable non-table catalog registries (graphs, views,
    /// schemas, analyzers, FDW objects, indexes, and SQL routines). Each SQL
    /// session owns a private cache and publishes this generation only after
    /// its outer transaction commits, so uncommitted catalog state cannot
    /// leak through shared in-memory maps.
    catalog_registry_epoch: Arc<std::sync::atomic::AtomicU64>,
    seen_catalog_registry_epoch: std::sync::atomic::AtomicU64,
    catalog_registry_dirty: AtomicBool,
    catalog_registry_refresh: parking_lot::Mutex<()>,
    /// Session-local cache of named graphs reachable from SQL via the
    /// `graph_*` function family. Durable definitions and graph contents are
    /// restored from the catalog and synchronized by `catalog_registry_epoch`.
    graphs: Arc<RwLock<BTreeMap<String, uqa_graph::MemoryGraphStore>>>,
    /// Saved deep-fusion models. Mirrors the catalog `_models` table
    /// when the engine is SQLite-backed.
    models: RwLock<BTreeMap<String, DeepModel>>,
    /// Bayesian calibration parameters (`alpha`, `beta`, `base_rate`, ...)
    /// keyed by signal name. Round-trips through the catalog when the
    /// engine is `SQLite`-backed.
    scoring_params: RwLock<BTreeMap<String, String>>,
    /// Persisted, fully lowered view definitions. Execution and catalog
    /// restoration both use the same `QueryPlan`; no AST carrier is kept.
    views: Arc<RwLock<BTreeMap<RelationIdentity, uqa_planner::QueryPlan>>>,
    /// Registered secondary indexes from `CREATE INDEX`.
    catalog_indexes: Arc<RwLock<BTreeMap<String, CatalogIndexRow>>>,
    /// Durable schema catalog. `public` is an ordinary, always-present
    /// catalog object; qualified relation creation validates against this set.
    schemas: Arc<RwLock<std::collections::BTreeSet<String>>>,
    /// Resolution order for unqualified table names. Mirrors the canonical UQA implementation's
    /// `Engine._tables._search_path`. Defaults to `["public"]`.
    search_path: RwLock<Vec<String>>,
    /// Session-scoped runtime parameters. Anything assigned via SET
    /// lands here so SHOW can echo it back; `DISCARD ALL` clears the
    /// map. Mirrors the canonical UQA implementation's `Engine._session_vars`.
    session_vars: RwLock<BTreeMap<String, String>>,
    /// Logical-session PRNG state used by SQL `random()` / `setseed()`.
    /// It is deliberately not shared by sibling `SQLite` sessions.
    random_state: parking_lot::Mutex<u64>,
    /// Pre-built RPQ path indexes keyed by `<graph>::<name>`. Each
    /// entry materialises a fixed set of label sequences so RPQ can
    /// short-circuit NFA simulation when the expression matches.
    path_indexes: Arc<RwLock<BTreeMap<String, uqa_graph::PathIndex>>>,
    /// Open transaction stack. `BEGIN` pushes a new frame, `COMMIT`
    /// / `ROLLBACK` pop one, savepoint statements update the top
    /// frame's savepoint set.
    tx_stack: parking_lot::Mutex<Vec<TransactionFrame>>,
    /// Per-engine cancellation token. Operators cloned through
    /// [`Engine::cancellation_token`] check the flag at chunk
    /// boundaries; calling [`Engine::cancel`] from any thread tears
    /// every in-flight query down with `SQLError::Cancelled`.
    cancel: uqa_core::CancellationToken,
    /// Named sequences. Mirrors `_engine._sequences` in the UQA implementation
    /// reference. Each entry tracks `(start, increment, current)`.
    sequences: RwLock<BTreeMap<RelationIdentity, SequenceState>>,
    /// Last sequence value observed by this logical SQL session. Durable
    /// sequence definitions and allocation state live in `sequences` and the
    /// catalog; `currval` must never leak a sibling session's allocation.
    sequence_currvals: RwLock<BTreeMap<RelationIdentity, i64>>,
    /// Prepared unified execution plans. Mirrors `_engine._prepared` while
    /// ensuring EXECUTE cannot re-enter an AST-only dispatch path.
    prepared: RwLock<BTreeMap<String, PreparedStatementPlan>>,
    /// Parsed single statements and their lowered logical plans, keyed by SQL
    /// text. In-memory read-only statements also retain the optimized plan;
    /// persistent statements replan after acquiring their storage snapshot.
    sql_statement_cache: RwLock<SQLStatementCache>,
    /// Named analyzers from `CREATE ANALYZER`. Stores the config
    /// JSON string for `list_analyzers` introspection. Mirrors
    /// `_engine.create_analyzer` / `drop_analyzer`.
    named_analyzers: Arc<RwLock<BTreeMap<String, String>>>,
    /// Per-(table, field) analyzer assignments from
    /// `set_table_analyzer`. Stores `(analyzer_name, phase)` so the
    /// FTS pipeline can pick up index-time vs query-time analyzers.
    table_field_analyzers: Arc<RwLock<TableFieldAnalyzerRegistry>>,
    /// `CREATE SERVER` registry. Keyed by server name; the value is
    /// the FDW handler descriptor.
    foreign_servers: Arc<RwLock<BTreeMap<String, uqa_fdw::ForeignServer>>>,
    /// `CREATE FOREIGN TABLE` registry. Keyed by table name.
    foreign_tables: Arc<RwLock<BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>>>,
    /// Row payloads for `memory_fdw` foreign tables. This keeps the
    /// reference FDW executable without pretending that memory rows are
    /// part of the persistent catalog.
    foreign_memory_tables: Arc<RwLock<BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>>>,
    /// Engine-local Rust scalar SQL functions. Rust API registrations are
    /// runtime configuration, not SQL-catalog state, and deliberately do not
    /// participate in SQL transaction/savepoint rollback.
    sql_scalar_functions:
        Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLScalarFunction>>>>,
    /// Engine-local Rust table SQL functions (non-transactional runtime
    /// configuration, shared by sibling sessions).
    sql_table_functions: Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLTableFunction>>>>,
    /// Engine-local Rust aggregate SQL functions (non-transactional runtime
    /// configuration, shared by sibling sessions).
    sql_aggregate_functions:
        Arc<RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLAggregateFunction>>>>,
    /// User-defined SQL / PL/pgSQL routines from `CREATE FUNCTION` /
    /// `CREATE PROCEDURE`, keyed by canonical qualified `schema.name`.
    /// Each entry holds the parameter-type overload set for that name.
    /// Definitions persist to catalog metadata; compiled bodies are rebuilt
    /// on restore.
    sql_user_functions:
        Arc<RwLock<BTreeMap<String, Vec<Arc<engine_user_functions::SQLUserFunction>>>>>,
    /// `RAISE NOTICE` / `WARNING` / ... sink: `(level, message)`
    /// pairs in emission order, drained by [`Engine::take_sql_notices`].
    sql_notices: parking_lot::Mutex<Vec<(String, String)>>,
    /// Nesting cap for user-defined routine calls. This Rust runtime setting is
    /// intentionally outside SQL transaction rollback.
    sql_function_depth_limit: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Default)]
struct SQLStatementCache {
    entries: BTreeMap<String, CachedSQLStatement>,
    insertion_order: VecDeque<String>,
}

#[derive(Clone)]
pub(crate) struct CachedSQLStatement {
    pub(crate) statement: Arc<uqa_sql::ast::Statement>,
    pub(crate) logical_plan: Arc<uqa_planner::UnifiedPlan>,
    pub(crate) optimized_plan: Option<Arc<uqa_planner::UnifiedPlan>>,
}

#[derive(Clone)]
struct PreparedStatementPlan {
    logical_plan: uqa_planner::UnifiedPlan,
    plan: uqa_planner::UnifiedPlan,
}

impl SQLStatementCache {
    fn get(&self, sql: &str) -> Option<CachedSQLStatement> {
        self.entries.get(sql).cloned()
    }

    fn insert(
        &mut self,
        sql: String,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        let cached = CachedSQLStatement {
            statement,
            logical_plan,
            optimized_plan: None,
        };
        if let Entry::Occupied(mut entry) = self.entries.entry(sql.clone()) {
            entry.insert(cached);
            return;
        }
        while self.entries.len() >= SQL_STATEMENT_CACHE_LIMIT {
            let Some(oldest) = self.insertion_order.pop_front() else {
                self.entries.clear();
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                break;
            }
        }
        self.insertion_order.push_back(sql.clone());
        self.entries.insert(sql, cached);
    }

    fn set_optimized(&mut self, sql: &str, optimized_plan: Arc<uqa_planner::UnifiedPlan>) {
        if let Some(entry) = self.entries.get_mut(sql) {
            entry.optimized_plan = Some(optimized_plan);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }
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

#[derive(Default)]
struct TransactionFrame {
    storage_savepoint: Option<String>,
    read_only: bool,
    savepoints: std::collections::BTreeSet<String>,
    session_snapshot: SessionStateSnapshot,
    session_savepoints: BTreeMap<String, SessionStateSnapshot>,
    data_snapshot: Option<EngineDataSnapshot>,
    data_savepoints: BTreeMap<String, EngineDataSnapshot>,
    dirty_at_begin: TransactionDirtyState,
    dirty_savepoints: BTreeMap<String, TransactionDirtyState>,
}

/// Lightweight SQL-session state that follows transaction/savepoint rollback
/// for every backend. It is intentionally separate from the database-sized
/// memory-engine snapshot so persistent sessions receive identical SET,
/// search-path, PRNG, sequence-currval, PREPARE, and statement-cache semantics.
#[derive(Clone, Default)]
struct SessionStateSnapshot {
    search_path: Vec<String>,
    session_vars: BTreeMap<String, String>,
    random_state: u64,
    sequence_currvals: BTreeMap<RelationIdentity, i64>,
    prepared: BTreeMap<String, PreparedStatementPlan>,
    sql_statement_cache: SQLStatementCache,
}

#[derive(Clone)]
struct EngineDataSnapshot {
    tables: BTreeMap<RelationIdentity, TableDataSnapshot>,
    graphs: BTreeMap<String, uqa_graph::MemoryGraphStore>,
    models: BTreeMap<String, DeepModel>,
    scoring_params: BTreeMap<String, String>,
    views: BTreeMap<RelationIdentity, uqa_planner::QueryPlan>,
    sequences: BTreeMap<RelationIdentity, SequenceState>,
    catalog_indexes: BTreeMap<String, CatalogIndexRow>,
    schemas: std::collections::BTreeSet<String>,
    path_indexes: BTreeMap<String, uqa_graph::PathIndex>,
    named_analyzers: BTreeMap<String, String>,
    table_field_analyzers: TableFieldAnalyzerRegistry,
    foreign_servers: BTreeMap<String, uqa_fdw::ForeignServer>,
    foreign_tables: BTreeMap<RelationIdentity, uqa_fdw::ForeignTable>,
    foreign_memory_tables: BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>,
    sql_user_functions: BTreeMap<String, Vec<Arc<engine_user_functions::SQLUserFunction>>>,
}

#[derive(Clone)]
struct TableDataSnapshot {
    state: Arc<TableState>,
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
    doc_count_cache: u64,
    doc_count_dirty: bool,
}

pub(crate) struct TableState {
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
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IVFIndexParams {
    pub nlist: usize,
    pub nprobe: usize,
    pub train_threshold: usize,
}

impl Default for IVFIndexParams {
    fn default() -> Self {
        Self {
            nlist: 100,
            nprobe: 10,
            train_threshold: 256,
        }
    }
}

impl IVFIndexParams {
    pub(crate) fn from_catalog_map(
        parameters: &BTreeMap<String, String>,
    ) -> StorageBackendResult<Self> {
        fn read_positive(
            parameters: &BTreeMap<String, String>,
            keys: &[&str],
            default: usize,
        ) -> StorageBackendResult<usize> {
            let Some((key, raw)) = parameters.iter().find(|(key, _)| {
                keys.iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
            }) else {
                return Ok(default);
            };
            let value = raw.parse::<usize>().map_err(|_| {
                StorageBackendError::Other(format!(
                    "invalid persisted IVF parameter `{key}` value `{raw}`"
                ))
            })?;
            if value == 0 {
                return Err(StorageBackendError::Other(format!(
                    "persisted IVF parameter `{key}` must be greater than zero"
                )));
            }
            Ok(value)
        }

        let default = Self::default();
        Ok(Self {
            nlist: read_positive(parameters, &["lists", "nlist"], default.nlist)?,
            nprobe: read_positive(parameters, &["probes", "nprobe"], default.nprobe)?,
            train_threshold: read_positive(
                parameters,
                &["train_threshold", "train-threshold", "min_train"],
                default.train_threshold,
            )?,
        })
    }
}

impl TableState {
    fn fts_fields(&self) -> Vec<FieldName> {
        self.fts_fields.read().clone()
    }
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

impl Engine {
    /// In-memory engine. State lives only as long as this `Engine`.
    pub fn new() -> Self {
        Self {
            statement_gate: parking_lot::ReentrantMutex::new(()),
            tables: RwLock::new(BTreeMap::new()),
            catalog: None,
            backend: None,
            sqlite_session: None,
            seen_sqlite_data_version: std::sync::atomic::AtomicU64::new(0),
            external_commit_refresh: parking_lot::Mutex::new(()),
            table_catalog_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_table_catalog_epoch: std::sync::atomic::AtomicU64::new(1),
            table_catalog_dirty: AtomicBool::new(false),
            table_catalog_refresh: parking_lot::Mutex::new(()),
            table_data_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_table_data_epoch: std::sync::atomic::AtomicU64::new(1),
            table_data_dirty: AtomicBool::new(false),
            table_data_refresh: parking_lot::Mutex::new(()),
            catalog_registry_epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            seen_catalog_registry_epoch: std::sync::atomic::AtomicU64::new(1),
            catalog_registry_dirty: AtomicBool::new(false),
            catalog_registry_refresh: parking_lot::Mutex::new(()),
            graphs: Arc::new(RwLock::new(BTreeMap::new())),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: Arc::new(RwLock::new(BTreeMap::new())),
            catalog_indexes: Arc::new(RwLock::new(BTreeMap::new())),
            schemas: Arc::new(RwLock::new(std::collections::BTreeSet::from([
                "public".to_string()
            ]))),
            search_path: RwLock::new(vec!["public".to_string()]),
            session_vars: RwLock::new(BTreeMap::new()),
            random_state: parking_lot::Mutex::new(initial_random_state()),
            path_indexes: Arc::new(RwLock::new(BTreeMap::new())),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
            sequences: RwLock::new(BTreeMap::new()),
            sequence_currvals: RwLock::new(BTreeMap::new()),
            prepared: RwLock::new(BTreeMap::new()),
            sql_statement_cache: RwLock::new(SQLStatementCache::default()),
            named_analyzers: Arc::new(RwLock::new(BTreeMap::new())),
            table_field_analyzers: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_servers: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_tables: Arc::new(RwLock::new(BTreeMap::new())),
            foreign_memory_tables: Arc::new(RwLock::new(BTreeMap::new())),
            sql_scalar_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_table_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_aggregate_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_user_functions: Arc::new(RwLock::new(BTreeMap::new())),
            sql_notices: parking_lot::Mutex::new(Vec::new()),
            sql_function_depth_limit: std::sync::atomic::AtomicUsize::new(SQL_FUNCTION_DEPTH_LIMIT),
        }
    }

    pub(crate) fn cached_sql_statement(&self, sql: &str) -> Option<CachedSQLStatement> {
        self.sql_statement_cache.read().get(sql)
    }

    pub(crate) fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        self.sql_statement_cache
            .write()
            .insert(sql, statement, logical_plan);
    }

    pub(crate) fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        self.sql_statement_cache
            .write()
            .set_optimized(sql, optimized_plan);
    }

    #[cfg(test)]
    pub(crate) fn cached_sql_plans(&self, sql: &str) -> Option<Vec<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql)
            .map(|cached| vec![cached.logical_plan.as_ref().clone()])
    }

    pub(crate) fn clear_sql_statement_cache(&self) {
        self.sql_statement_cache.write().clear();
    }

    // -----------------------------------------------------------------
    // Rust SQL function registry. Registered functions are engine-local
    // runtime objects; they are not persisted to the catalog.
    // -----------------------------------------------------------------
}

fn default_runtime_parameter(name: &str) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("server_version") {
        return Some("17.0-uqa");
    }
    if name.eq_ignore_ascii_case("server_encoding") || name.eq_ignore_ascii_case("client_encoding")
    {
        return Some("UTF8");
    }
    if name.eq_ignore_ascii_case("datestyle") {
        return Some("ISO, MDY");
    }
    if name.eq_ignore_ascii_case("timezone") {
        return Some("UTC");
    }
    if name.eq_ignore_ascii_case("work_mem") {
        return Some("64MB");
    }
    None
}

fn is_known_runtime_parameter(name: &str) -> bool {
    name.eq_ignore_ascii_case("search_path") || default_runtime_parameter(name).is_some()
}

fn is_mutable_runtime_parameter(name: &str) -> bool {
    name.eq_ignore_ascii_case("search_path")
        || name.eq_ignore_ascii_case("client_encoding")
        || name.eq_ignore_ascii_case("datestyle")
        || name.eq_ignore_ascii_case("timezone")
        || name.eq_ignore_ascii_case("work_mem")
}

fn initial_random_state() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STATE: AtomicU64 = AtomicU64::new(0x4d59_5df4_d0f3_3173);
    NEXT_STATE.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
}

impl uqa_sql::expr::EngineHook for Engine {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String> {
        Engine::nextval(self, name)
    }
    fn currval(&self, name: &str) -> std::result::Result<i64, String> {
        Engine::currval(self, name)
    }
    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String> {
        Engine::setval(self, name, value)
    }
    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        self.call_registered_scalar_function(name, args)
    }
    fn has_scalar_functions(&self) -> bool {
        self.has_registered_scalar_functions()
    }
    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.current_schema_name()
            .map_err(|error| error.to_string())
    }
    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.current_schema_names(include_implicit)
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(Some(self.next_random_value()))
    }
    fn set_random_seed(&self, seed: f64) -> std::result::Result<bool, String> {
        Engine::set_random_seed(self, seed)?;
        Ok(true)
    }
    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_user_scalar_function(self, name, args)
    }
}

/// Bundle of hybrid-search arguments. Keeps [`Engine::hybrid_search`]
/// borrowing-friendly without an explosion of positional parameters.
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
    /// Confidence-scaling exponent for robust positive-evidence pooling.
    pub alpha: f64,
    pub top_k: usize,
}

fn value_to_f64_vec(value: &Value) -> Result<Vec<f64>, String> {
    match value {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Float(value) => Ok(*value),
                Value::Int(value) => Ok(*value as f64),
                Value::Decimal(value) => value
                    .to_f64()
                    .ok_or_else(|| "decimal feature is outside f64 range".to_string()),
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        other => Err(format!("expected feature array, got {other:?}")),
    }
}

fn value_to_usize(value: &Value) -> Result<usize, String> {
    match value {
        Value::Int(value) if *value >= 0 => usize::try_from(*value)
            .map_err(|_| format!("integer label {value} exceeds the platform usize range")),
        Value::Float(value) => {
            let exponent = i32::try_from(usize::BITS)
                .map_err(|_| "platform usize width exceeds f64 exponent range".to_string())?;
            let upper_exclusive = 2.0_f64.powi(exponent);
            if !value.is_finite()
                || *value < 0.0
                || value.fract() != 0.0
                || *value >= upper_exclusive
            {
                return Err(format!(
                    "expected finite non-negative integer label within usize range, got {value}"
                ));
            }
            Ok(*value as usize)
        }
        other => Err(format!(
            "expected non-negative integer label, got {other:?}"
        )),
    }
}

// -----------------------------------------------------------------
// ANALYZE helpers (mirror Table._build_histogram / _build_mcv).
// -----------------------------------------------------------------

const HISTOGRAM_BUCKETS: usize = 100;
const MCV_COUNT: usize = 10;

fn distinct_count(values: &[Value]) -> StorageBackendResult<u64> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<&Value> = BTreeSet::new();
    for v in values {
        set.insert(v);
    }
    u64::try_from(set.len())
        .map_err(|_| StorageBackendError::Other("ANALYZE distinct count exceeds u64".into()))
}

fn build_histogram(values: &[&Value]) -> Vec<Value> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<Value> = values.iter().map(|v| (*v).clone()).collect();
    sorted.sort();
    let n = sorted.len();
    let num_buckets = HISTOGRAM_BUCKETS.min(n);
    if num_buckets <= 1 {
        return vec![sorted[0].clone(), sorted[n - 1].clone()];
    }
    let mut boundaries: Vec<Value> = vec![sorted[0].clone()];
    for i in 1..num_buckets {
        let idx = (i * n) / num_buckets;
        let val = &sorted[idx];
        if Some(val) != boundaries.last() {
            boundaries.push(val.clone());
        }
    }
    if boundaries.last() != Some(&sorted[n - 1]) {
        boundaries.push(sorted[n - 1].clone());
    }
    boundaries
}

fn build_mcv(values: &[Value], total: u64) -> (Vec<Value>, Vec<f64>) {
    if values.is_empty() || total == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut counts: BTreeMap<&Value, u64> = BTreeMap::new();
    for v in values {
        *counts.entry(v).or_insert(0) += 1;
    }
    let ndv = counts.len();
    if ndv == 0 {
        return (Vec::new(), Vec::new());
    }
    let avg_freq = 1.0 / ndv as f64;
    let mut sorted: Vec<(&Value, u64)> = counts.into_iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let total_f = total as f64;
    let mut mcv_values: Vec<Value> = Vec::new();
    let mut mcv_freqs: Vec<f64> = Vec::new();
    for (val, cnt) in sorted.into_iter().take(MCV_COUNT) {
        let freq = cnt as f64 / total_f;
        if freq > avg_freq {
            mcv_values.push(val.clone());
            mcv_freqs.push(freq);
        }
    }
    (mcv_values, mcv_freqs)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
