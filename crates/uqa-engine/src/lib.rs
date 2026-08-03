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
//! - [`Engine::sql_cursor`] / [`Engine::sql_columnar`] - bounded, schema-ordered
//!   column batches for result sets that should not be retained in memory.
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
mod engine_state;
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
    HNSWIndex, HNSWIndexParams, IVFIndex, IVFIndexParams, InvertedIndex, ManagedConnection,
    MemoryDocumentStore, MemoryInvertedIndex, MemoryVectorIndex, PersistentStorageBackend,
    RelationIdentity, SQLiteCompressedContainerAnchor, SQLiteStorageBackend, SequenceRow,
    StorageBackendError, StorageBackendResult, TableSchema, VectorFieldSchema, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec, ViewRow,
};

pub use sql::{SQLCursor, SQLCursorSummary};
pub use uqa_execution::{ColumnVector, ColumnarBatch};
pub use uqa_sql::{ast::SequenceRestart, SQLParam, SQLResult};
pub use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions, SQLiteError};

use engine_state::{
    DurableCatalogSnapshot, DurableCatalogState, EpochCoordinator, QueryRuntime, RuntimeExtensions,
    SessionContext, StorageContext,
};
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

/// Unified query engine composed from explicit storage, durable-catalog,
/// session, extension, epoch, and query-runtime ownership domains.
pub struct Engine {
    storage: StorageContext,
    durable: DurableCatalogState,
    session: SessionContext,
    extensions: RuntimeExtensions,
    epochs: EpochCoordinator,
    runtime: QueryRuntime,
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
    durable: DurableCatalogSnapshot,
    foreign_memory_tables: BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>,
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
            storage: StorageContext::memory(),
            durable: DurableCatalogState::new(),
            session: SessionContext::new(initial_random_state()),
            extensions: RuntimeExtensions::new(),
            epochs: EpochCoordinator::new(),
            runtime: QueryRuntime::new(SQL_FUNCTION_DEPTH_LIMIT),
        }
    }

    pub(crate) fn cached_sql_statement(&self, sql: &str) -> Option<CachedSQLStatement> {
        self.session.state.read().sql_statement_cache.get(sql)
    }

    pub(crate) fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        self.session
            .state
            .write()
            .sql_statement_cache
            .insert(sql, statement, logical_plan);
    }

    pub(crate) fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        self.session
            .state
            .write()
            .sql_statement_cache
            .set_optimized(sql, optimized_plan);
    }

    #[cfg(test)]
    pub(crate) fn cached_sql_plans(&self, sql: &str) -> Option<Vec<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql)
            .map(|cached| vec![cached.logical_plan.as_ref().clone()])
    }

    pub(crate) fn clear_sql_statement_cache(&self) {
        self.session.state.write().sql_statement_cache.clear();
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
