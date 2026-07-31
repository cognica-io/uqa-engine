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
mod tests {
    use super::*;

    #[test]
    fn value_to_usize_rejects_non_finite_fractional_and_out_of_range_floats() {
        assert_eq!(value_to_usize(&Value::Float(42.0)).unwrap(), 42);
        for value in [f64::NAN, f64::INFINITY, -1.0, 1.5] {
            assert!(value_to_usize(&Value::Float(value)).is_err());
        }
        let exponent = i32::try_from(usize::BITS).unwrap();
        assert!(value_to_usize(&Value::Float(2.0_f64.powi(exponent))).is_err());
    }

    #[test]
    fn persisted_ivf_parameters_reject_invalid_values() {
        let invalid = BTreeMap::from([("lists".to_string(), "not-a-number".to_string())]);
        assert!(IVFIndexParams::from_catalog_map(&invalid).is_err());

        let zero = BTreeMap::from([("probes".to_string(), "0".to_string())]);
        assert!(IVFIndexParams::from_catalog_map(&zero).is_err());
    }

    #[test]
    fn document_id_watermark_represents_and_reports_exhaustion_without_wrapping() {
        let engine = Engine::new();
        engine.create_default_table("docs", Vec::new()).unwrap();
        let table = engine.table("docs").unwrap().expect("table");
        *table.next_id.lock() = u128::from(u64::MAX);

        assert_eq!(engine.allocate_next_id("docs").unwrap(), u64::MAX);
        let error = engine.allocate_next_id("docs").unwrap_err();
        assert!(error.to_string().contains("document id space"), "{error}");
        assert_eq!(*table.next_id.lock(), u128::from(u64::MAX) + 1);

        let second = Engine::new();
        second.create_default_table("docs", Vec::new()).unwrap();
        second.advance_next_id("docs", u64::MAX).unwrap();
        let error = second.allocate_next_id("docs").unwrap_err();
        assert!(error.to_string().contains("document id space"), "{error}");
    }

    #[test]
    fn vector_backfill_reports_invalid_values_instead_of_skipping_them() {
        let engine = Engine::new();
        engine.create_default_table("docs", Vec::new()).unwrap();
        engine
            .add_document(
                "docs",
                1,
                doc([("embedding", Value::Str("not-a-vector".into()))]),
            )
            .unwrap();

        let error = engine
            .create_vector_field("docs", "embedding", 2)
            .unwrap_err();
        assert!(error.to_string().contains("expected vector"), "{error}");
        let unregistered = engine
            .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
            .unwrap_err();
        assert!(matches!(unregistered, SQLError::TypeMismatch(_)));
    }

    #[test]
    fn vector_field_registration_distinguishes_absence_noop_and_dimension_mismatch() {
        let engine = Engine::new();
        let missing = engine
            .create_vector_field("missing", "embedding", 2)
            .unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");

        engine.create_default_table("docs", Vec::new()).unwrap();
        assert!(engine.create_vector_field("docs", "embedding", 2).unwrap());
        assert!(!engine.create_vector_field("docs", "embedding", 2).unwrap());
        let mismatch = engine
            .create_vector_field("docs", "embedding", 3)
            .unwrap_err();
        assert!(mismatch.to_string().contains("dimension 2"), "{mismatch}");
        assert!(mismatch.to_string().contains("requested 3"), "{mismatch}");
    }

    #[test]
    fn direct_vector_writes_reject_unknown_tables_and_unregistered_fields() {
        let engine = Engine::new();
        let missing = engine
            .add_vector("missing", 1, "embedding", vec![1.0, 0.0])
            .unwrap_err();
        assert!(matches!(missing, SQLError::UnknownTable(_)));

        engine.create_default_table("docs", Vec::new()).unwrap();
        let unregistered = engine
            .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
            .unwrap_err();
        assert!(matches!(unregistered, SQLError::TypeMismatch(_)));
        let unregistered_many = engine
            .add_vector_values("docs", 1, "embedding", vec![vec![1.0, 0.0]])
            .unwrap_err();
        assert!(matches!(unregistered_many, SQLError::TypeMismatch(_)));

        assert!(engine.create_vector_field("docs", "embedding", 2).unwrap());
        assert!(engine
            .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
            .unwrap());
        assert!(engine
            .add_vector_values("docs", 1, "embedding", vec![vec![0.0, 1.0]])
            .unwrap());
    }

    #[test]
    fn table_introspection_distinguishes_unknown_tables_from_missing_columns() {
        let engine = Engine::new();
        for error in [
            engine.try_table_columns("missing").unwrap_err(),
            engine.try_table_has_column("missing", "value").unwrap_err(),
            engine.column_type("missing", "value").unwrap_err(),
        ] {
            assert!(error.to_string().contains("does not exist"), "{error}");
        }

        engine.create_default_table("docs", Vec::new()).unwrap();
        assert!(engine.try_table_columns("docs").unwrap().is_empty());
        assert!(!engine.try_table_has_column("docs", "value").unwrap());
        assert_eq!(engine.column_type("docs", "value").unwrap(), None);

        let sql_error = engine.sql("SELECT * FROM missing", &[]).unwrap_err();
        assert!(sql_error.to_string().contains("missing"), "{sql_error}");
        assert!(
            sql_error.to_string().contains("does not exist"),
            "{sql_error}"
        );
    }

    #[test]
    fn direct_schema_mutations_reject_missing_relations_columns_and_duplicates() {
        let engine = Engine::new();
        let column = uqa_sql::ast::ColumnDef {
            name: "value".into(),
            ty: uqa_sql::ast::ColumnType::Integer,
            primary_key: false,
            not_null: false,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            references: None,
        };

        for error in [
            engine
                .register_column("missing", column.clone())
                .unwrap_err(),
            engine
                .set_column_default("missing", "value", None)
                .unwrap_err(),
            engine
                .set_column_not_null("missing", "value", true)
                .unwrap_err(),
            engine
                .set_column_type("missing", "value", &uqa_sql::ast::ColumnType::Boolean)
                .unwrap_err(),
            engine
                .try_column_default_expr("missing", "value")
                .unwrap_err(),
            engine.advance_next_id("missing", 1).unwrap_err(),
            engine
                .refresh_value_indexes_for_table("missing")
                .unwrap_err(),
            engine.try_persist_table_schema("missing").unwrap_err(),
            engine
                .try_rebuild_vector_index_for_column("missing", "embedding", 2)
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("does not exist"), "{error}");
        }

        engine.create_default_table("docs", Vec::new()).unwrap();
        engine.register_column("docs", column.clone()).unwrap();
        let duplicate = engine.register_column("docs", column).unwrap_err();
        assert!(
            duplicate.to_string().contains("already exists"),
            "{duplicate}"
        );
        for error in [
            engine
                .set_column_default("docs", "absent", None)
                .unwrap_err(),
            engine
                .set_column_not_null("docs", "absent", true)
                .unwrap_err(),
            engine
                .set_column_type("docs", "absent", &uqa_sql::ast::ColumnType::Boolean)
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("column `absent`"), "{error}");
        }

        assert!(engine.set_column_default("docs", "value", None).unwrap());
        assert!(engine.set_column_not_null("docs", "value", true).unwrap());
        assert!(engine
            .set_column_type("docs", "value", &uqa_sql::ast::ColumnType::Boolean)
            .unwrap());
        assert_eq!(
            engine.column_type("docs", "value").unwrap(),
            Some(uqa_sql::ast::ColumnType::Boolean)
        );
    }

    #[test]
    fn table_metadata_getters_reject_unknown_relations() {
        let engine = Engine::new();
        assert!(engine.describe_table("missing").unwrap().is_none());
        for error in [
            engine.auto_increment_column("missing").unwrap_err(),
            engine.try_check_constraints("missing").unwrap_err(),
            engine.try_foreign_keys("missing").unwrap_err(),
            engine.try_unique_columns("missing").unwrap_err(),
            engine.try_key_constraints("missing").unwrap_err(),
            engine.try_referrers_to("missing").unwrap_err(),
            engine.try_column_stats("missing").unwrap_err(),
        ] {
            assert!(error.to_string().contains("does not exist"), "{error}");
        }

        engine.create_default_table("docs", Vec::new()).unwrap();
        assert_eq!(engine.auto_increment_column("docs").unwrap(), None);
        assert!(engine.try_check_constraints("docs").unwrap().is_empty());
        assert!(engine.try_foreign_keys("docs").unwrap().is_empty());
        assert!(engine.try_unique_columns("docs").unwrap().is_empty());
        assert!(engine.try_key_constraints("docs").unwrap().is_empty());
        assert!(engine.try_referrers_to("docs").unwrap().is_empty());
        assert!(engine.try_column_stats("docs").unwrap().is_empty());
    }

    #[test]
    fn document_mutations_distinguish_unknown_tables_from_missing_documents() {
        let engine = Engine::new();
        let updates = BTreeMap::from([("value".to_string(), Value::Int(1))]);
        let vectors = BTreeMap::new();
        for error in [
            engine
                .update_document_fields("missing", 1, updates.clone(), vectors.clone())
                .unwrap_err(),
            engine
                .patch_document_fields("missing", 1, &updates, &vectors)
                .unwrap_err(),
            engine
                .rewrite_document("missing", 1, Document::new())
                .unwrap_err(),
            engine.delete_document("missing", 1).unwrap_err(),
        ] {
            assert!(matches!(error, SQLError::UnknownTable(_)), "{error}");
        }

        engine.create_default_table("docs", Vec::new()).unwrap();
        assert!(!engine
            .update_document_fields("docs", 1, updates.clone(), vectors.clone())
            .unwrap());
        assert!(!engine
            .patch_document_fields("docs", 1, &updates, &vectors)
            .unwrap());
        engine.delete_document("docs", 1).unwrap();
    }

    #[test]
    fn tensor_backfill_reports_inner_dimension_mismatch_and_allows_null() {
        let tensor_column = uqa_sql::ast::ColumnDef {
            name: "embedding".into(),
            ty: uqa_sql::ast::ColumnType::Tensor(2),
            primary_key: false,
            not_null: false,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            references: None,
        };

        let engine = Engine::new();
        engine.create_default_table("bad", Vec::new()).unwrap();
        engine
            .register_column("bad", tensor_column.clone())
            .unwrap();
        engine
            .add_document(
                "bad",
                1,
                doc([(
                    "embedding",
                    Value::List(vec![Value::List(vec![Value::Float(1.0)])]),
                )]),
            )
            .unwrap();
        let error = engine
            .create_vector_field("bad", "embedding", 2)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("vector dimension mismatch: expected 2, got 1"),
            "{error}"
        );

        let nullable = Engine::new();
        nullable
            .create_default_table("nullable", Vec::new())
            .unwrap();
        nullable.register_column("nullable", tensor_column).unwrap();
        nullable
            .add_document("nullable", 1, doc([("embedding", Value::Null)]))
            .unwrap();
        assert!(nullable
            .create_vector_field("nullable", "embedding", 2)
            .unwrap());
    }

    fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn vector(values: &[f64]) -> Value {
        Value::List(values.iter().copied().map(Value::Float).collect())
    }

    fn vector_index_kind(engine: &Engine, table: &str, field: &str) -> String {
        let table = engine.table(table).unwrap().expect("table");
        let indexes = table.vector_indexes.read();
        indexes
            .get(field)
            .expect("vector index")
            .index_kind()
            .into()
    }

    #[derive(Clone)]
    struct StoreWithMissingDocId {
        docs: BTreeMap<DocId, Document>,
        missing_doc_id: DocId,
        read_snapshot_calls: Arc<std::sync::atomic::AtomicUsize>,
        writable_snapshot_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl StoreWithMissingDocId {
        fn from_table(engine: &Engine, table: &str, missing_doc_id: DocId) -> Self {
            let table = engine.table(table).unwrap().expect("table");
            let docs = table.document_store.read().iter_all().unwrap().collect();
            Self {
                docs,
                missing_doc_id,
                read_snapshot_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                writable_snapshot_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
    }

    impl DocumentStore for StoreWithMissingDocId {
        fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
            self.docs.insert(doc_id, document);
            Ok(())
        }

        fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
            Ok(self.docs.get(&doc_id).cloned())
        }

        fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
            self.docs.remove(&doc_id);
            Ok(())
        }

        fn clear(&mut self) -> StorageBackendResult<()> {
            self.docs.clear();
            Ok(())
        }

        fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
            let mut ids = vec![self.missing_doc_id];
            ids.extend(self.docs.keys().copied());
            Ok(ids)
        }

        fn len(&self) -> StorageBackendResult<usize> {
            Ok(self.docs.len() + 1)
        }

        fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
            self.read_snapshot_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Arc::new(self.clone()))
        }

        fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
            self.writable_snapshot_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Box::new(self.clone()))
        }
    }

    #[test]
    fn transaction_snapshot_captures_one_writable_copy_without_probe_clone() {
        let eng = Engine::new();
        eng.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        let store = StoreWithMissingDocId::from_table(&eng, "docs", 99);
        let read_calls = store.read_snapshot_calls.clone();
        let writable_calls = store.writable_snapshot_calls.clone();
        {
            let table = eng.table("docs").unwrap().expect("table");
            *table.document_store.write() = Box::new(store);
        }

        eng.begin().unwrap();
        eng.commit().unwrap();

        assert_eq!(read_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(writable_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn sql_update_reports_stale_document_ids() {
        let eng = Engine::new();
        eng.sql(
            "CREATE TABLE docs (
               id INTEGER PRIMARY KEY,
               status TEXT,
               title TEXT,
               content TEXT
             )",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX docs_fts ON docs USING gin (title, content)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, status, title, content)
             VALUES (1, 'queued', 'Runtime search', 'old content'),
                    (2, 'indexed', 'Other', 'other content')",
            &[],
        )
        .unwrap();
        {
            let table = eng.table("docs").unwrap().expect("table");
            *table.document_store.write() =
                Box::new(StoreWithMissingDocId::from_table(&eng, "docs", 99));
        }

        let error = eng
            .sql(
                "UPDATE docs
                    SET content = 'updated content',
                        status = 'indexed'
                  WHERE id = 1 AND status = 'queued'",
                &[],
            )
            .expect_err("a stale index candidate must not be treated as no matching row");

        assert!(error
            .to_string()
            .contains("candidate 99 is missing from the document-field snapshot for table `docs`"));
        let doc = eng.get_document("docs", 1).unwrap().unwrap();
        assert_eq!(doc.get("content"), Some(&s("old content")));
        assert_eq!(doc.get("status"), Some(&s("queued")));
    }

    /// Document store whose next `fail_budget` put calls fail. Used to
    /// prove that an ON CONFLICT DO UPDATE whose delete succeeded but
    /// whose re-insert failed surfaces the error and rolls back instead
    /// of committing the row away (the Maek `global_config` loss shape).
    #[derive(Clone)]
    struct FailingPutStore {
        docs: BTreeMap<DocId, Document>,
        fail_budget: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FailingPutStore {
        fn from_table(engine: &Engine, table: &str, fail_budget: usize) -> Self {
            let table = engine.table(table).unwrap().expect("table");
            let docs = table.document_store.read().iter_all().unwrap().collect();
            Self {
                docs,
                fail_budget: Arc::new(std::sync::atomic::AtomicUsize::new(fail_budget)),
            }
        }
    }

    impl DocumentStore for FailingPutStore {
        fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
            let remaining = self.fail_budget.load(Ordering::SeqCst);
            if remaining > 0 {
                self.fail_budget.store(remaining - 1, Ordering::SeqCst);
                return Err(StorageBackendError::Other(
                    "injected put failure".to_string(),
                ));
            }
            self.docs.insert(doc_id, document);
            Ok(())
        }

        fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
            Ok(self.docs.get(&doc_id).cloned())
        }

        fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
            self.docs.remove(&doc_id);
            Ok(())
        }

        fn clear(&mut self) -> StorageBackendResult<()> {
            self.docs.clear();
            Ok(())
        }

        fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
            Ok(self.docs.keys().copied().collect())
        }

        fn len(&self) -> StorageBackendResult<usize> {
            Ok(self.docs.len())
        }

        fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
            Ok(Arc::new(self.clone()))
        }

        fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
            Ok(Box::new(self.clone()))
        }
    }

    #[test]
    fn upsert_reinsert_failure_rolls_back_instead_of_losing_the_row() {
        let eng = Engine::new();
        eng.sql(
            "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v1')",
            &[],
        )
        .unwrap();
        {
            let table = eng.table("engine_meta").unwrap().expect("table");
            *table.document_store.write() =
                Box::new(FailingPutStore::from_table(&eng, "engine_meta", 1));
        }

        let err = eng
            .sql(
                "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v2') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
                &[],
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("document store write failed"),
            "unexpected error: {err}"
        );

        // The row must survive with its previous value: the failed
        // rewrite has to roll back, not commit its delete half.
        let result = eng
            .sql(
                "SELECT value FROM engine_meta WHERE key = 'global_config'",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("value"), Some(&s("v1")));

        // With the fault budget exhausted the same upsert succeeds.
        eng.sql(
            "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v2') \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[],
        )
        .unwrap();
        let result = eng
            .sql(
                "SELECT value FROM engine_meta WHERE key = 'global_config'",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows[0].get("value"), Some(&s("v2")));
    }

    #[test]
    fn persistent_engine_restores_through_facade_traits() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("facade.db");
        let conn = ManagedConnection::open(&db).unwrap();
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(conn.clone()).unwrap());
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(conn.clone()));

        {
            let eng = Engine::from_persistent_backends(catalog.clone(), backend.clone()).unwrap();
            eng.create_default_table("docs", vec!["title".into()])
                .unwrap();
            eng.add_document("docs", 1, doc([("title", s("hello facade"))]))
                .unwrap();
        }

        let reopened = Engine::from_persistent_backends(catalog, backend).unwrap();
        assert_eq!(reopened.document_count("docs").unwrap(), 1);
        let hits = reopened
            .search("docs", "title", "facade", &ScoringMode::default(), 10)
            .unwrap();
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
    }

    fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn normalise(v: &mut [f32]) {
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if mag > 1e-12 {
            for x in v {
                *x /= mag;
            }
        }
    }

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    fn nearest_and_other_ivf_centroid(conn: &ManagedConnection, query: &[f32]) -> (i64, i64) {
        let mut query = query.to_vec();
        normalise(&mut query);
        conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT centroid_id, vector FROM _ivf_centroids
                  WHERE table_name = 'public.articles' AND field = 'embedding'
                  ORDER BY centroid_id",
            )?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
            let mut centroids = Vec::new();
            for row in rows {
                let (id, blob) = row?;
                let mut centroid = blob_to_vector(&blob);
                normalise(&mut centroid);
                centroids.push((id, centroid));
            }
            assert!(centroids.len() >= 2);
            let nearest = centroids
                .iter()
                .max_by(|(_, a), (_, b)| {
                    dot(&query, a)
                        .partial_cmp(&dot(&query, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(id, _)| *id)
                .unwrap();
            let other = centroids
                .iter()
                .map(|(id, _)| *id)
                .find(|id| *id != nearest)
                .unwrap();
            Ok((nearest, other))
        })
        .unwrap()
    }

    fn make_doc_two_the_only_nearest_ivf_candidate(conn: &ManagedConnection, query: &[f32]) {
        let (nearest, other) = nearest_and_other_ivf_centroid(conn, query);
        conn.with(|conn| {
            conn.execute(
                "DELETE FROM _ivf_assignments
                  WHERE table_name = 'public.articles' AND field = 'embedding'",
                [],
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO _ivf_assignments
                        (table_name, field, doc_id, centroid_id)
                     VALUES ('public.articles', 'embedding', 1, {other})"
                ),
                [],
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO _ivf_assignments
                        (table_name, field, doc_id, centroid_id)
                     VALUES ('public.articles', 'embedding', 2, {nearest})"
                ),
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn stored_vector(conn: &ManagedConnection, doc_id: DocId) -> Vec<f32> {
        conn.with(|conn| {
            let blob: Vec<u8> = conn.query_row(
                "SELECT vector FROM _vectors
                  WHERE table_name = 'public.articles'
                    AND field = 'embedding'
                    AND doc_id = ?1
                  ORDER BY vector_ordinal
                  LIMIT 1",
                [doc_id as i64],
                |r| r.get(0),
            )?;
            Ok(blob_to_vector(&blob))
        })
        .unwrap()
    }

    #[test]
    fn run_analyze_populates_column_stats() {
        let eng = Engine::new();
        eng.create_default_table("docs", vec!["title".into()])
            .unwrap();
        // Register the columns directly through the table state so we
        // don't depend on the SQL DDL path here.
        if let Some(t) = eng.table("docs").unwrap() {
            *t.columns.write() = vec![uqa_sql::ast::ColumnDef {
                name: "title".into(),
                ty: uqa_sql::ast::ColumnType::Text,
                primary_key: false,
                not_null: false,
                auto_increment: false,
                unique: false,
                default: None,
                check: None,
                references: None,
            }];
        }
        eng.add_document("docs", 1, doc([("title", s("alpha"))]))
            .unwrap();
        eng.add_document("docs", 2, doc([("title", s("alpha"))]))
            .unwrap();
        eng.add_document("docs", 3, doc([("title", s("beta"))]))
            .unwrap();
        eng.run_analyze(Some("docs")).unwrap();
        let stats = eng.column_stats("docs").unwrap();
        let title_stats = stats.get("title").expect("title stats");
        assert_eq!(title_stats.row_count, 3);
        assert_eq!(title_stats.distinct_count, 2);
        assert_eq!(title_stats.null_count, 0);
        // "alpha" appears twice and dominates the MCV list.
        assert_eq!(title_stats.mcv_values.first(), Some(&s("alpha")));
    }

    #[test]
    fn add_get_delete_round_trip() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.add_document("articles", 1, doc([("title", s("rust language"))]))
            .unwrap();
        let got = eng.get_document("articles", 1).unwrap().unwrap();
        assert_eq!(got.get("title"), Some(&s("rust language")));
        eng.delete_document("articles", 1).unwrap();
        assert!(eng.get_document("articles", 1).unwrap().is_none());
    }

    #[test]
    fn search_returns_top_k_bm25_in_score_order() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.add_document(
            "articles",
            1,
            doc([("title", s("the rust programming language"))]),
        )
        .unwrap();
        eng.add_document("articles", 2, doc([("title", s("python language guide"))]))
            .unwrap();
        eng.add_document("articles", 3, doc([("title", s("rust rust rust"))]))
            .unwrap();

        let hits = eng
            .search(
                "articles",
                "title",
                "rust",
                &ScoringMode::BM25(BM25Params::default()),
                10,
            )
            .unwrap();
        // Doc 3 has tf=3 and is shorter -> highest BM25.
        assert_eq!(hits.first().map(|h| h.doc_id), Some(3));
        assert!(hits.iter().any(|h| h.doc_id == 1));
        assert!(hits.iter().all(|h| h.doc_id != 2));
    }

    #[test]
    fn search_top_k_matches_full_score_prefix() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        for doc_id in 1..=20 {
            let body = std::iter::repeat_n("rust", doc_id as usize)
                .collect::<Vec<_>>()
                .join(" ");
            eng.add_document("articles", doc_id, doc([("title", s(&body))]))
                .unwrap();
        }

        let full = eng
            .search(
                "articles",
                "title",
                "rust",
                &ScoringMode::BM25(BM25Params::default()),
                usize::MAX,
            )
            .unwrap();
        let top = eng
            .search(
                "articles",
                "title",
                "rust",
                &ScoringMode::BM25(BM25Params::default()),
                3,
            )
            .unwrap();

        assert_eq!(top.len(), 3);
        assert_eq!(
            top.iter().map(|hit| hit.doc_id).collect::<Vec<_>>(),
            full.iter()
                .take(3)
                .map(|hit| hit.doc_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_returns_calibrated_probabilities_under_bayesian_bm25() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.add_document(
            "articles",
            1,
            doc([("title", s("the rust programming language"))]),
        )
        .unwrap();
        eng.add_document("articles", 2, doc([("title", s("python is dynamic"))]))
            .unwrap();

        let hits = eng
            .search(
                "articles",
                "title",
                "rust",
                &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
                10,
            )
            .unwrap();

        // Bayesian BM25 always returns probabilities in (0, 1).
        for h in &hits {
            assert!(
                (0.0..=1.0).contains(&h.score),
                "score {} out of [0, 1]",
                h.score
            );
        }
        assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
    }

    #[test]
    fn knn_returns_top_k_in_descending_similarity() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.create_vector_field("articles", "embedding", 3).unwrap();
        eng.add_document_with_vectors(
            "articles",
            1,
            doc([("title", s("a"))]),
            BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
        )
        .unwrap();
        eng.add_document_with_vectors(
            "articles",
            2,
            doc([("title", s("b"))]),
            BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
        )
        .unwrap();
        eng.add_document_with_vectors(
            "articles",
            3,
            doc([("title", s("c"))]),
            BTreeMap::from([("embedding".into(), vec![0.7, 0.7, 0.0])]),
        )
        .unwrap();

        let hits = eng
            .knn_search("articles", "embedding", vec![1.0, 0.0, 0.0], 2)
            .unwrap();
        assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
        // doc 3 (cos ~0.707) beats doc 2 (cos 0.0).
        assert_eq!(hits.get(1).map(|h| h.doc_id), Some(3));
    }

    #[test]
    fn vector_fields_use_bruteforce_until_explicit_ivf_index() {
        let eng = Engine::new();
        eng.sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
            &[],
        )
        .unwrap();
        assert_eq!(
            vector_index_kind(&eng, "articles", "embedding"),
            "memory-bruteforce"
        );
        eng.sql(
            "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding)",
            &[],
        )
        .unwrap();
        assert_eq!(vector_index_kind(&eng, "articles", "embedding"), "ivf");

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vectors.db");
        {
            let eng = Engine::open(&db).unwrap();
            eng.sql(
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
                &[],
            )
            .unwrap();
            assert_eq!(
                vector_index_kind(&eng, "articles", "embedding"),
                "sqlite-bruteforce"
            );
            eng.sql(
                "CREATE INDEX articles_embedding_ivf ON articles USING ivf (embedding)",
                &[],
            )
            .unwrap();
            assert_eq!(
                vector_index_kind(&eng, "articles", "embedding"),
                "sqlite-ivf"
            );
        }

        let eng = Engine::open(&db).unwrap();
        assert_eq!(
            vector_index_kind(&eng, "articles", "embedding"),
            "sqlite-ivf"
        );
    }

    #[test]
    fn sqlite_ivf_restore_reuses_persisted_assignments() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vectors.db");
        {
            let eng = Engine::open(&db).unwrap();
            eng.sql(
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
                &[],
            )
            .unwrap();
            eng.sql(
                "INSERT INTO articles (id, embedding) VALUES \
                 (1, ARRAY[1.0, 0.0]), \
                 (2, ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();
            eng.sql(
                "CREATE INDEX articles_embedding_ivf ON articles USING hnsw (embedding) \
                 WITH (lists = 2, probes = 1, train_threshold = 2)",
                &[],
            )
            .unwrap();

            let conn = ManagedConnection::open(&db).unwrap();
            make_doc_two_the_only_nearest_ivf_candidate(&conn, &[1.0, 0.0]);
        }

        let reopened = Engine::open(&db).unwrap();
        assert_eq!(
            vector_index_kind(&reopened, "articles", "embedding"),
            "sqlite-ivf"
        );
        let hits = reopened
            .knn_search("articles", "embedding", vec![1.0, 0.0], 1)
            .unwrap();
        assert_eq!(hits.first().map(|h| h.doc_id), Some(2));
    }

    #[test]
    fn sqlite_ivf_create_index_reuses_existing_persistent_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vectors.db");
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE articles (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO articles (id, embedding) VALUES \
             (1, ARRAY[1.0, 0.0]), \
             (2, ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();

        let conn = ManagedConnection::open(&db).unwrap();
        conn.with(|conn| {
            conn.execute(
                "UPDATE _documents
                    SET body = json_set(body, '$.embedding', json('[0.0, 1.0]'))
                  WHERE table_name = 'public.articles' AND doc_id = 1",
                [],
            )?;
            conn.execute(
                "UPDATE _documents
                    SET body = json_set(body, '$.embedding', json('[1.0, 0.0]'))
                  WHERE table_name = 'public.articles' AND doc_id = 2",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        eng.sql(
            "CREATE INDEX articles_embedding_ivf ON articles USING hnsw (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();

        assert_eq!(stored_vector(&conn, 1), vec![1.0, 0.0]);
        assert_eq!(stored_vector(&conn, 2), vec![0.0, 1.0]);
    }

    #[test]
    fn create_vector_field_backfills_existing_documents() {
        let eng = Engine::new();
        eng.create_default_table("docs", vec![]).unwrap();
        eng.add_document("docs", 1, doc([("embedding", vector(&[1.0, 0.0]))]))
            .unwrap();
        eng.add_document("docs", 2, doc([("embedding", vector(&[0.0, 1.0]))]))
            .unwrap();
        eng.add_document("docs", 3, doc([("embedding", vector(&[0.8, 0.2]))]))
            .unwrap();

        assert!(eng.create_vector_field("docs", "embedding", 2).unwrap());
        let hits = eng
            .knn_search("docs", "embedding", vec![1.0, 0.0], 2)
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.doc_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn hybrid_search_combines_text_and_vector_signals() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        eng.create_vector_field("articles", "embedding", 3).unwrap();

        // Doc 1: title matches "rust", embedding pointing toward query.
        eng.add_document_with_vectors(
            "articles",
            1,
            doc([("title", s("rust language"))]),
            BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
        )
        .unwrap();
        // Doc 2: title matches "rust", embedding orthogonal to query.
        eng.add_document_with_vectors(
            "articles",
            2,
            doc([("title", s("rust ecosystem"))]),
            BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
        )
        .unwrap();
        // Doc 3: no text match, embedding near query.
        eng.add_document_with_vectors(
            "articles",
            3,
            doc([("title", s("python programming"))]),
            BTreeMap::from([("embedding".into(), vec![0.95, 0.1, 0.0])]),
        )
        .unwrap();

        let hits = eng
            .hybrid_search(&HybridSearchParams {
                table: "articles",
                text_field: "title",
                text_query: "rust",
                vector_field: "embedding",
                query_vector: vec![1.0, 0.0, 0.0],
                knn_pool: 10,
                alpha: 0.5,
                top_k: 10,
            })
            .unwrap();

        // Doc 1 should rank highest: text match AND high cosine.
        assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
        // All three should appear (after coverage-based defaults fill
        // missing signals).
        let ids: Vec<DocId> = hits.iter().map(|h| h.doc_id).collect();
        assert!(ids.contains(&1) && ids.contains(&2) && ids.contains(&3));
    }

    #[test]
    fn document_count_tracks_indexed_documents() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()])
            .unwrap();
        for i in 0..5 {
            eng.add_document("articles", i, doc([("title", s(&format!("doc {i}")))]))
                .unwrap();
        }
        assert_eq!(eng.document_count("articles").unwrap(), 5);
    }
}
