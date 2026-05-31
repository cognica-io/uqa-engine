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
//!   functions: `text_match`, `knn_match`, `fuse_log_odds`,
//!   `multi_field_match`, `staged_retrieval`, `graph_*`, `deep_predict`).
//! - [`Engine::search`] - direct text-only retrieval returning a posting
//!   list.
//! - [`Engine::knn_search`], [`Engine::vector_similarity_search`] - k-NN
//!   over a vector field.
//! - [`Engine::hybrid_search`] - log-odds fusion of text and vector
//!   posting lists (no SQL parsing in the hot path).
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

pub mod migration;
pub mod operator_tree_bridge;
pub mod sql;

use std::collections::BTreeMap;
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
use uqa_operators::{
    CosineProbabilityOperator, ExecutionContext, KNNOperator, LogOddsFusionOperator, Operator,
    ScoreOperator, TermOperator, VectorSimilarityOperator,
};
use uqa_scoring::{
    BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics,
    CalibrationReport, ParameterLearner, Scorer,
};
use uqa_sql::SQLError;
use uqa_storage::{
    document_store::Document, AnalyzerPhase, Catalog, CatalogFacade, CatalogIndexRow,
    ColumnStatsInput, ColumnStatsRow, DocumentStore, IVFIndex, InvertedIndex, ManagedConnection,
    MemoryDocumentStore, MemoryInvertedIndex, PersistentStorageBackend,
    PersistentVectorIndexParams, SQLiteCompressionOptions, SQLiteError, SQLiteStorageBackend,
    StorageBackendError, StorageBackendResult, TableSchema, VectorFieldSchema, VectorIndex,
};

pub use uqa_sql::{SQLParam, SQLResult};

const SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";
const VIEWS_METADATA_KEY: &str = "sql_views_json";

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

/// Engine: per-table document store + inverted index + vector indexes,
/// each behind a `RwLock<Box<dyn ...>>` so the `Memory*` and `SQLite*`
/// backends drop in interchangeably.
pub struct Engine {
    tables: RwLock<BTreeMap<String, Arc<TableState>>>,
    catalog: Option<Arc<dyn CatalogFacade>>,
    backend: Option<Arc<dyn PersistentStorageBackend>>,
    /// Named in-memory graphs reachable from SQL via the
    /// `graph_*` function family. Persistence to the catalog is left
    /// to a follow-up slice.
    graphs: RwLock<BTreeMap<String, uqa_graph::MemoryGraphStore>>,
    /// Saved deep-fusion models. Mirrors the catalog `_models` table
    /// when the engine is SQLite-backed.
    models: RwLock<BTreeMap<String, DeepModel>>,
    /// Bayesian calibration parameters (`alpha`, `beta`, `base_rate`, ...)
    /// keyed by signal name. Round-trips through the catalog when the
    /// engine is `SQLite`-backed.
    scoring_params: RwLock<BTreeMap<String, String>>,
    /// Registered views. Each entry holds the underlying
    /// `SelectStmt`; the SQL surface re-runs the body on every
    /// reference (no row caching).
    views: RwLock<BTreeMap<String, uqa_sql::ast::SelectStmt>>,
    /// Registered secondary indexes from `CREATE INDEX`.
    catalog_indexes: RwLock<BTreeMap<String, CatalogIndexRow>>,
    /// Registered schema names. Engine-level schemas are advisory
    /// today: the catalog records them so `CREATE SCHEMA` does not
    /// error out, but tables themselves still live in the flat
    /// per-name namespace.
    schemas: RwLock<std::collections::BTreeSet<String>>,
    /// Resolution order for unqualified table names. Mirrors the canonical UQA implementation's
    /// `Engine._tables._search_path`. Defaults to `["public"]`.
    search_path: RwLock<Vec<String>>,
    /// Session-scoped runtime parameters. Anything assigned via SET
    /// lands here so SHOW can echo it back; `DISCARD ALL` clears the
    /// map. Mirrors the canonical UQA implementation's `Engine._session_vars`.
    session_vars: RwLock<BTreeMap<String, String>>,
    /// Pre-built RPQ path indexes keyed by `<graph>::<name>`. Each
    /// entry materialises a fixed set of label sequences so RPQ can
    /// short-circuit NFA simulation when the expression matches.
    path_indexes: RwLock<BTreeMap<String, uqa_graph::PathIndex>>,
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
    sequences: RwLock<BTreeMap<String, SequenceState>>,
    /// Prepared statements. Mirrors `_engine._prepared`.
    prepared: RwLock<BTreeMap<String, uqa_sql::ast::Statement>>,
    /// Named analyzers from `CREATE ANALYZER`. Stores the config
    /// JSON string for `list_analyzers` introspection. Mirrors
    /// `_engine.create_analyzer` / `drop_analyzer`.
    named_analyzers: RwLock<BTreeMap<String, String>>,
    /// Per-(table, field) analyzer assignments from
    /// `set_table_analyzer`. Stores `(analyzer_name, phase)` so the
    /// FTS pipeline can pick up index-time vs query-time analyzers.
    table_field_analyzers: RwLock<BTreeMap<(String, String), (String, String)>>,
    /// `CREATE SERVER` registry. Keyed by server name; the value is
    /// the FDW handler descriptor.
    foreign_servers: RwLock<BTreeMap<String, uqa_fdw::ForeignServer>>,
    /// `CREATE FOREIGN TABLE` registry. Keyed by table name.
    foreign_tables: RwLock<BTreeMap<String, uqa_fdw::ForeignTable>>,
    /// Row payloads for `memory_fdw` foreign tables. This keeps the
    /// reference FDW executable without pretending that memory rows are
    /// part of the persistent catalog.
    foreign_memory_tables: RwLock<BTreeMap<String, Vec<uqa_fdw::Row>>>,
}

/// Mutable state of a single SQL sequence.
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
pub struct SequenceState {
    pub start: i64,
    pub increment: i64,
    pub current: i64,
}

#[derive(Default)]
struct TransactionFrame {
    storage_savepoint: Option<String>,
    savepoints: std::collections::BTreeSet<String>,
    data_snapshot: Option<EngineDataSnapshot>,
    data_savepoints: BTreeMap<String, EngineDataSnapshot>,
}

#[derive(Clone)]
struct EngineDataSnapshot {
    tables: BTreeMap<String, TableDataSnapshot>,
    sequences: BTreeMap<String, SequenceState>,
}

#[derive(Clone)]
struct TableDataSnapshot {
    state: Arc<TableState>,
    documents: Vec<(DocId, Document)>,
    next_id: u64,
}

struct TableState {
    document_store: RwLock<Box<dyn DocumentStore>>,
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
    next_id: parking_lot::Mutex<u64>,
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
    pub(crate) fn from_map_lossy(parameters: &BTreeMap<String, String>) -> Self {
        fn read_positive(
            parameters: &BTreeMap<String, String>,
            keys: &[&str],
            default: usize,
        ) -> usize {
            parameters
                .iter()
                .find(|(key, _)| keys.iter().any(|k| key.eq_ignore_ascii_case(k)))
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(default)
        }

        let default = Self::default();
        Self {
            nlist: read_positive(parameters, &["lists", "nlist"], default.nlist),
            nprobe: read_positive(parameters, &["probes", "nprobe"], default.nprobe),
            train_threshold: read_positive(
                parameters,
                &["train_threshold", "train-threshold", "min_train"],
                default.train_threshold,
            ),
        }
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
    serde_json::from_value(value)
        .map_err(|e| format!("analyzer `{name}` config is not a valid analyzer: {e}"))
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
            tables: RwLock::new(BTreeMap::new()),
            catalog: None,
            backend: None,
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            catalog_indexes: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(std::collections::BTreeSet::new()),
            search_path: RwLock::new(vec!["public".to_string()]),
            session_vars: RwLock::new(BTreeMap::new()),
            path_indexes: RwLock::new(BTreeMap::new()),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
            sequences: RwLock::new(BTreeMap::new()),
            prepared: RwLock::new(BTreeMap::new()),
            named_analyzers: RwLock::new(BTreeMap::new()),
            table_field_analyzers: RwLock::new(BTreeMap::new()),
            foreign_servers: RwLock::new(BTreeMap::new()),
            foreign_tables: RwLock::new(BTreeMap::new()),
            foreign_memory_tables: RwLock::new(BTreeMap::new()),
        }
    }

    // -----------------------------------------------------------------
    // Sequences. Mirrors `_engine._sequences` and the
    // `_compile_create_sequence` / `_compile_alter_sequence` paths in
    // the canonical UQA behavior.
    // -----------------------------------------------------------------

    pub fn create_sequence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        if_not_exists: bool,
    ) -> bool {
        let name = self.relation_name_for_create(name);
        let mut seqs = self.sequences.write();
        if seqs.contains_key(&name) {
            return if_not_exists;
        }
        seqs.insert(
            name,
            SequenceState {
                start,
                increment,
                current: start - increment,
            },
        );
        drop(seqs);
        self.persist_sequences();
        true
    }

    pub fn alter_sequence(
        &self,
        name: &str,
        restart: Option<Option<i64>>,
        increment: Option<i64>,
        start: Option<i64>,
    ) -> Result<(), String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        if let Some(start_val) = start {
            seq.start = start_val;
        }
        if let Some(inc) = increment {
            seq.increment = inc;
        }
        if let Some(opt) = restart {
            let restart_val = opt.unwrap_or(seq.start);
            seq.current = restart_val - seq.increment;
        }
        drop(seqs);
        self.persist_sequences();
        Ok(())
    }

    pub fn drop_sequence(&self, name: &str) -> bool {
        let Some(name) = self.resolve_sequence_name(name) else {
            return false;
        };
        let removed = self.sequences.write().remove(&name).is_some();
        if removed {
            self.persist_sequences();
        }
        removed
    }

    pub fn nextval(&self, name: &str) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        seq.current += seq.increment;
        let current = seq.current;
        drop(seqs);
        self.persist_sequences();
        Ok(current)
    }

    pub fn currval(&self, name: &str) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let seqs = self.sequences.read();
        seqs.get(&name)
            .map(|s| s.current)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))
    }

    pub fn setval(&self, name: &str, value: i64) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        seq.current = value;
        drop(seqs);
        self.persist_sequences();
        Ok(value)
    }

    /// Snapshot of all registered sequences as `(name, state)` pairs.
    pub fn sequences_snapshot(&self) -> BTreeMap<String, SequenceState> {
        self.sequences.read().clone()
    }

    /// Resolve a sequence name through the current `search_path` and return
    /// its canonical name with the current state.
    pub fn sequence_state(&self, name: &str) -> Option<(String, SequenceState)> {
        let canonical = self.resolve_sequence_name(name)?;
        let seqs = self.sequences.read();
        seqs.get(&canonical)
            .copied()
            .map(|state| (canonical, state))
    }

    fn persist_sequences(&self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        if let Ok(json) = serde_json::to_string(&*self.sequences.read()) {
            let _ = catalog.set_metadata(SEQUENCES_METADATA_KEY, &json);
        }
    }

    fn restore_sequences_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(SEQUENCES_METADATA_KEY)? else {
            return Ok(());
        };
        if let Ok(sequences) = serde_json::from_str::<BTreeMap<String, SequenceState>>(&json) {
            *self.sequences.write() = sequences;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Prepared statements. Mirrors `_engine._prepared`.
    // -----------------------------------------------------------------

    pub fn register_prepared(&self, name: String, stmt: uqa_sql::ast::Statement) {
        self.prepared.write().insert(name, stmt);
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_sql::ast::Statement> {
        self.prepared.read().get(name).cloned()
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(n) => {
                self.prepared.write().remove(n);
            }
            None => self.prepared.write().clear(),
        }
    }

    // -----------------------------------------------------------------
    // Named analyzers + per-(table, field) analyzer assignments. Mirror
    // _engine.create_analyzer / drop_analyzer / list_analyzers /
    // set_table_analyzer in the canonical UQA behavior.
    // -----------------------------------------------------------------

    fn resolve_analyzer(&self, name: &str) -> std::result::Result<Analyzer, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("analyzer name cannot be empty".into());
        }
        if let Ok(analyzer) = analyzer_registry::get_analyzer(name) {
            return Ok(analyzer);
        }
        let Some(config_json) = self.named_analyzers.read().get(name).cloned() else {
            return Err(format!("analyzer `{name}` is not registered"));
        };
        parse_analyzer_config(name, &config_json)
    }

    pub fn register_named_analyzer(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        let _ = parse_analyzer_config(name, config_json)?;
        self.named_analyzers
            .write()
            .insert(name.to_string(), config_json.to_string());
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_analyzer(name, config_json);
        }
        Ok(())
    }

    pub fn drop_named_analyzer(&self, name: &str) -> bool {
        let removed = self.named_analyzers.write().remove(name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_analyzer(name);
            }
        }
        removed
    }

    pub fn list_named_analyzers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.named_analyzers.read().keys().cloned().collect();
        names.sort();
        names
    }

    pub fn set_table_field_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        let Some(t) = self.table(table) else {
            return Err(format!(
                "set_table_analyzer: table `{table}` does not exist"
            ));
        };
        let analyzer = self.resolve_analyzer(analyzer_name)?;
        let (phase_name, phase) = normalize_analyzer_phase(phase)?;
        t.inverted_index
            .write()
            .set_field_analyzer(field, analyzer, phase)
            .map_err(|e| format!("set_table_analyzer: {e}"))?;
        self.table_field_analyzers.write().insert(
            (table.to_string(), field.to_string()),
            (analyzer_name.to_string(), phase_name.clone()),
        );
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_table_field_analyzer(table, field, &phase_name, analyzer_name);
        }
        if matches!(phase, AnalyzerPhase::Index | AnalyzerPhase::Both)
            && t.fts_fields().iter().any(|f| f == field)
        {
            Self::rebuild_fts_index(&t)?;
        }
        Ok(())
    }

    pub fn table_field_analyzer(&self, table: &str, field: &str) -> Option<(String, String)> {
        self.table_field_analyzers
            .read()
            .get(&(table.to_string(), field.to_string()))
            .cloned()
    }

    /// compatibility alias for [`Engine::register_named_analyzer`].
    pub fn create_analyzer(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        self.register_named_analyzer(name, config_json)
    }

    /// compatibility alias for [`Engine::drop_named_analyzer`].
    pub fn drop_analyzer(&self, name: &str) -> bool {
        self.drop_named_analyzer(name)
    }

    /// compatibility alias for [`Engine::set_table_field_analyzer`].
    pub fn set_table_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        self.set_table_field_analyzer(table, field, analyzer_name, phase)
    }

    /// Resolve the analyzer assigned to `(table, field)` for the given
    /// phase. `phase` is `"index"`, `"search"`, or `"both"`. Returns the
    /// analyzer config JSON (the raw form the engine persists).
    /// Mirrors the canonical UQA implementation's `Engine.get_table_analyzer`.
    pub fn get_table_analyzer(&self, table: &str, field: &str, phase: &str) -> Option<String> {
        let (name, stored_phase) = self.table_field_analyzer(table, field)?;
        // Resolve the field's index/search analyzer based on the requested
        // phase; "both" means the override applies on both sides.
        let resolved = match (stored_phase.as_str(), phase) {
            ("both", _) | ("index", "index") | ("query" | "search", "search") => name,
            _ => return None,
        };
        self.resolve_analyzer(&resolved)
            .ok()
            .and_then(|analyzer| serde_json::to_string(&analyzer).ok())
    }

    // -----------------------------------------------------------------
    // Foreign Data Wrapper registry. Mirrors `_engine._foreign_servers`
    // / `_engine._foreign_tables` in the canonical UQA behavior.
    // -----------------------------------------------------------------

    #[allow(clippy::needless_pass_by_value)]
    pub fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        let mut servers = self.foreign_servers.write();
        if servers.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign server `{name}` already exists"));
        }
        if !matches!(fdw_type.as_str(), "duckdb_fdw" | "arrow_fdw" | "memory_fdw") {
            return Err(format!("Unsupported FDW type: `{fdw_type}`"));
        }
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        servers.insert(
            name.clone(),
            uqa_fdw::ForeignServer {
                name: name.clone(),
                fdw_type: fdw_type.clone(),
                options: opt_map.clone(),
            },
        );
        drop(servers);
        if let Some(catalog) = self.catalog.as_ref() {
            let options_json = serde_json::to_string(&opt_map).unwrap_or_else(|_| "{}".into());
            let _ = catalog.save_foreign_server(&name, &fdw_type, &options_json);
        }
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn register_foreign_table(
        &self,
        name: String,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        let name = self.relation_name_for_create(&name);
        if self.has_table(&name) {
            return Err(format!("Table `{name}` already exists"));
        }
        let mut tables = self.foreign_tables.write();
        if tables.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign table `{name}` already exists"));
        }
        if !self.foreign_servers.read().contains_key(&server_name) {
            return Err(format!("Foreign server `{server_name}` does not exist"));
        }
        let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
            .iter()
            .map(|c| uqa_fdw::ColumnDef {
                name: c.name.clone(),
                ty: match &c.ty {
                    uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
                    uqa_sql::ast::ColumnType::Real | uqa_sql::ast::ColumnType::Numeric { .. } => {
                        uqa_fdw::ColumnType::Real
                    }
                    uqa_sql::ast::ColumnType::Text
                    | uqa_sql::ast::ColumnType::Json
                    | uqa_sql::ast::ColumnType::Date
                    | uqa_sql::ast::ColumnType::Time
                    | uqa_sql::ast::ColumnType::TimeTz
                    | uqa_sql::ast::ColumnType::Timestamp
                    | uqa_sql::ast::ColumnType::TimestampTz => uqa_fdw::ColumnType::Text,
                    uqa_sql::ast::ColumnType::Bytea
                    | uqa_sql::ast::ColumnType::Vector(_)
                    | uqa_sql::ast::ColumnType::Tensor(_) => uqa_fdw::ColumnType::Bytes,
                },
            })
            .collect();
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        tables.insert(
            name.clone(),
            uqa_fdw::ForeignTable {
                name: name.clone(),
                server_name: server_name.clone(),
                columns: fdw_columns,
                options: opt_map.clone(),
            },
        );
        drop(tables);
        if let Some(catalog) = self.catalog.as_ref() {
            let columns_json = serde_json::to_string(&columns).unwrap_or_else(|_| "[]".into());
            let options_json = serde_json::to_string(&opt_map).unwrap_or_else(|_| "{}".into());
            let _ = catalog.save_foreign_table(&name, &server_name, &columns_json, &options_json);
        }
        Ok(())
    }

    pub fn drop_foreign_server(&self, name: &str) -> bool {
        // Reject when any foreign table references this server.
        let referenced = self
            .foreign_tables
            .read()
            .values()
            .any(|t| t.server_name == name);
        if referenced {
            return false;
        }
        let removed = self.foreign_servers.write().remove(name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_foreign_server(name);
            }
        }
        removed
    }

    pub fn drop_foreign_table(&self, name: &str) -> bool {
        let Some(name) = self.resolve_foreign_table_name(name) else {
            return false;
        };
        let removed = self.foreign_tables.write().remove(&name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_foreign_table(&name);
            }
        }
        removed
    }

    pub fn foreign_server(&self, name: &str) -> Option<uqa_fdw::ForeignServer> {
        self.foreign_servers.read().get(name).cloned()
    }

    pub fn foreign_table(&self, name: &str) -> Option<uqa_fdw::ForeignTable> {
        let resolved = self.resolve_foreign_table_name(name)?;
        self.foreign_tables.read().get(&resolved).cloned()
    }

    pub fn list_foreign_servers(&self) -> Vec<String> {
        let mut out: Vec<String> = self.foreign_servers.read().keys().cloned().collect();
        out.sort();
        out
    }

    pub fn list_foreign_tables(&self) -> Vec<String> {
        let mut out: Vec<String> = self.foreign_tables.read().keys().cloned().collect();
        out.sort();
        out
    }

    pub fn foreign_table_columns(&self, table: &str) -> Vec<String> {
        self.foreign_table(table)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn load_memory_foreign_table(
        &self,
        table_name: impl Into<String>,
        rows: Vec<uqa_fdw::Row>,
    ) -> std::result::Result<(), String> {
        let table_name = table_name.into();
        let table_name = self
            .resolve_foreign_table_name(&table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let table = self
            .foreign_table(&table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;
        if server.fdw_type != "memory_fdw" {
            return Err(format!(
                "Foreign table `{table_name}` is backed by `{}` not `memory_fdw`",
                server.fdw_type
            ));
        }
        self.foreign_memory_tables.write().insert(table_name, rows);
        Ok(())
    }

    pub(crate) fn scan_foreign_table(
        &self,
        table_name: &str,
        columns: Option<&[String]>,
        predicates: &[uqa_fdw::FDWPredicate],
        limit: Option<u64>,
    ) -> std::result::Result<Vec<uqa_fdw::Row>, String> {
        use uqa_fdw::FDWHandler as _;

        let table = self
            .foreign_table(table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;

        let rows = match server.fdw_type.as_str() {
            "memory_fdw" => {
                let mut handler = uqa_fdw::MemoryHandler::new();
                let rows = self
                    .foreign_memory_tables
                    .read()
                    .get(table_name)
                    .cloned()
                    .unwrap_or_default();
                handler.load(table_name, rows);
                handler.scan(&table, columns, predicates, limit)
            }
            "duckdb_fdw" => {
                let handler = uqa_fdw::DuckDBHandler::new(server);
                handler.scan(&table, columns, predicates, limit)
            }
            "arrow_fdw" => {
                let handler = uqa_fdw::ArrowIpcHandler::new(server);
                handler.scan(&table, columns, predicates, limit)
            }
            other => return Err(format!("Unsupported FDW type: `{other}`")),
        };
        rows.map_err(|err| err.to_string())
    }

    // -----------------------------------------------------------------
    // Catalog index registry. Mirrors the canonical UQA implementation's `_catalog_indexes`
    // table - records the CREATE INDEX statement (name + access
    // method + columns + WITH options) so reopen can replay any
    // metadata-bearing side effects.
    // -----------------------------------------------------------------

    pub fn register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) {
        let table = self
            .resolve_table_name(table)
            .unwrap_or_else(|| table.to_string());
        let columns_json = serde_json::to_string(columns).unwrap_or_else(|_| "[]".into());
        let options_map: std::collections::BTreeMap<String, String> =
            options.iter().cloned().collect();
        let parameters_json = serde_json::to_string(&options_map).unwrap_or_else(|_| "{}".into());
        self.catalog_indexes.write().insert(
            name.to_string(),
            CatalogIndexRow {
                name: name.to_string(),
                index_type: index_type.to_string(),
                table_name: table.clone(),
                columns_json: columns_json.clone(),
                parameters_json: parameters_json.clone(),
            },
        );
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_catalog_index(
                name,
                index_type,
                &table,
                &columns_json,
                &parameters_json,
            );
        }
    }

    pub fn drop_catalog_index(&self, name: &str) {
        self.catalog_indexes.write().remove(name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_catalog_index(name);
        }
    }

    pub fn has_catalog_index(&self, name: &str) -> bool {
        self.catalog_indexes.read().contains_key(name)
    }

    /// Cancel every in-flight query that holds a clone of this
    /// engine's cancellation token. Mirrors `Engine.cancel()` in
    /// the canonical UQA behavior; surfaces to operator hot loops as
    /// [`uqa_core::QueryCancelled`] which the SQL layer maps to
    /// `SQLError::Cancelled` (`PostgreSQL` `SQLSTATE 57014`).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Reset the cancellation flag so subsequent queries run
    /// normally. Call between query batches when reusing the same
    /// engine for many cancellable executions.
    pub fn reset_cancellation(&self) {
        self.cancel.reset();
    }

    pub fn cancellation_token(&self) -> uqa_core::CancellationToken {
        self.cancel.clone()
    }

    /// compatibility alias for [`Engine::cancellation_token`].
    pub fn cancel_token(&self) -> uqa_core::CancellationToken {
        self.cancellation_token()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// SQLite-backed engine. Opens (or creates) the database at `path`,
    /// runs catalog migrations, and rebuilds the in-memory table
    /// registry from the persisted catalog.
    pub fn open(path: &Path) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open(path)?;
        Self::open_with_connection(&conn)
    }

    /// SQLCipher-backed engine. Applies `key` before any catalog
    /// access, runs migrations, and rebuilds the in-memory table
    /// registry from the encrypted catalog.
    pub fn open_encrypted(path: &Path, key: &str) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_encrypted(path, key)?;
        Self::open_with_connection(&conn)
    }

    /// Compressed SQLite-backed engine. The compression VFS is
    /// schema-neutral: it compresses `SQLite` byte ranges in chunks
    /// without knowledge of UQA catalog tables or columns.
    pub fn open_compressed(
        path: &Path,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed(path, compression)?;
        Self::open_with_connection(&conn)
    }

    /// Compressed and encrypted SQLite-backed engine. Chunk payloads
    /// are compressed first, then encrypted by the compressed VFS.
    pub fn open_compressed_encrypted(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
    ) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open_compressed_encrypted(path, key, compression)?;
        Self::open_with_connection(&conn)
    }

    fn open_with_connection(conn: &ManagedConnection) -> Result<Self, SQLiteError> {
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(conn.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(conn.clone()));
        Self::from_persistent_backends(catalog, backend).map_err(Self::sqlite_open_error)
    }

    /// Build an engine from already-open persistent metadata and data
    /// backends. This is the storage-neutral entry point used by
    /// `Engine::open` after it creates the `SQLite` implementations,
    /// and by future `RocksDB` / `redb` constructors once they provide
    /// the same facade objects.
    pub fn from_persistent_backends(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> StorageBackendResult<Self> {
        let mut engine = Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog),
            backend: Some(backend),
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            scoring_params: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            catalog_indexes: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(std::collections::BTreeSet::new()),
            search_path: RwLock::new(vec!["public".to_string()]),
            session_vars: RwLock::new(BTreeMap::new()),
            path_indexes: RwLock::new(BTreeMap::new()),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
            sequences: RwLock::new(BTreeMap::new()),
            prepared: RwLock::new(BTreeMap::new()),
            named_analyzers: RwLock::new(BTreeMap::new()),
            table_field_analyzers: RwLock::new(BTreeMap::new()),
            foreign_servers: RwLock::new(BTreeMap::new()),
            foreign_tables: RwLock::new(BTreeMap::new()),
            foreign_memory_tables: RwLock::new(BTreeMap::new()),
        };
        let catalog = engine.catalog.as_ref().expect("persistent catalog").clone();
        let backend = engine.backend.as_ref().expect("persistent backend").clone();
        engine.restore_from_catalog(catalog.as_ref(), backend.as_ref())?;
        // Eagerly populate the model cache from the catalog so
        // `load_model` is one read deep.
        if let Ok(rows) = catalog.load_models() {
            for (name, json) in rows {
                if let Ok(model) = serde_json::from_str::<DeepModel>(&json) {
                    engine.models.write().insert(name, model);
                }
            }
        }
        Ok(engine)
    }

    fn sqlite_open_error(err: StorageBackendError) -> SQLiteError {
        match err {
            StorageBackendError::SQLite(err) => err,
            StorageBackendError::Serde(err) => SQLiteError::Serde(err),
            StorageBackendError::Other(msg) => SQLiteError::StorageBackend(msg),
        }
    }

    fn restore_from_catalog(
        &mut self,
        catalog: &dyn CatalogFacade,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        let schemas = catalog.load_tables()?;
        for schema in schemas {
            let analyzer: Analyzer = serde_json::from_str(&schema.analyzer_json)?;
            let docs = backend.document_store(&schema.name);
            let inv = backend.inverted_index(&schema.name, analyzer.clone());
            let mut vectors: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::new();
            for vf in &schema.vector_fields {
                vectors.insert(
                    vf.field.clone(),
                    backend.vector_index(
                        &schema.name,
                        &vf.field,
                        vf.dimensions,
                        Some(PersistentVectorIndexParams {
                            initialize: false,
                            ..PersistentVectorIndexParams::default()
                        }),
                    ),
                );
            }
            let columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&schema.columns_json).unwrap_or_default()
            };
            // Restore the per-table id watermark to one past the largest
            // existing doc id so reopened catalogs do not collide on
            // SERIAL/BIGSERIAL columns.
            let max_id = { docs.max_doc_id() };
            let table = TableState {
                document_store: RwLock::new(docs),
                inverted_index: RwLock::new(inv),
                vector_indexes: RwLock::new(vectors),
                fts_fields: RwLock::new(schema.fts_fields.clone()),
                columns: RwLock::new(columns),
                next_id: parking_lot::Mutex::new(max_id + 1),
                analyzer: RwLock::new(analyzer),
                column_stats: RwLock::new(BTreeMap::new()),
                column_stats_loaded: AtomicBool::new(false),
                column_stats_dirty: AtomicBool::new(false),
                table_checks: RwLock::new(Vec::new()),
                foreign_keys: RwLock::new(Vec::new()),
            };
            self.tables.write().insert(schema.name, Arc::new(table));
        }
        self.restore_graphs_from_catalog(catalog)?;
        self.restore_engine_registries_from_catalog(catalog)?;
        Ok(())
    }

    fn load_column_stats_from_catalog(
        catalog: &dyn CatalogFacade,
        table_name: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        let mut out = BTreeMap::new();
        for row in catalog.load_column_stats(table_name)? {
            out.insert(row.column_name.clone(), Self::column_stats_from_row(row));
        }
        Ok(out)
    }

    fn column_stats_from_row(row: ColumnStatsRow) -> uqa_planner::ColumnStats {
        uqa_planner::ColumnStats {
            distinct_count: row.distinct_count.try_into().unwrap_or(0),
            null_count: row.null_count.try_into().unwrap_or(0),
            min_value: Self::decode_column_stat_value(row.min_value),
            max_value: Self::decode_column_stat_value(row.max_value),
            row_count: row.row_count.try_into().unwrap_or(0),
            histogram: serde_json::from_str(&row.histogram_json).unwrap_or_default(),
            mcv_values: serde_json::from_str(&row.mcv_values_json).unwrap_or_default(),
            mcv_frequencies: serde_json::from_str(&row.mcv_frequencies_json).unwrap_or_default(),
        }
    }

    fn decode_column_stat_value(raw: Option<String>) -> Option<Value> {
        let raw = raw?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Null) => None,
            Ok(v) => Some(v),
            Err(_) => Some(Value::Str(raw)),
        }
    }

    /// Re-hydrate the named-analyzer / table-field-analyzer / foreign
    /// server / foreign table / catalog index / path index registries
    /// from the catalog. Mirrors the side effects of every
    /// `register_*` method but skips their catalog write-back so the
    /// load is idempotent.
    fn restore_engine_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        self.restore_sequences_from_metadata(catalog)?;
        self.restore_views_from_metadata(catalog)?;
        self.restore_analyzers_from_catalog(catalog)?;
        self.restore_foreign_registries_from_catalog(catalog)?;
        self.restore_catalog_indexes_from_catalog(catalog)?;
        self.restore_path_indexes_from_catalog(catalog)?;
        Ok(())
    }

    fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, config_json) in catalog.load_analyzers()? {
            self.named_analyzers.write().insert(name, config_json);
        }
        for (table, field, phase, analyzer_name) in catalog.load_table_field_analyzers()? {
            if let (Some(t), Ok(analyzer), Ok((phase_name, phase))) = (
                self.table(&table),
                self.resolve_analyzer(&analyzer_name),
                normalize_analyzer_phase(&phase),
            ) {
                let _ = t
                    .inverted_index
                    .write()
                    .set_field_analyzer(&field, analyzer, phase);
                self.table_field_analyzers
                    .write()
                    .insert((table, field), (analyzer_name, phase_name));
                continue;
            }
            self.table_field_analyzers
                .write()
                .insert((table, field), (analyzer_name, phase));
        }
        Ok(())
    }

    fn restore_foreign_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, fdw_type, options_json) in catalog.load_foreign_servers()? {
            let options: BTreeMap<String, String> =
                serde_json::from_str(&options_json).unwrap_or_default();
            self.foreign_servers.write().insert(
                name.clone(),
                uqa_fdw::ForeignServer {
                    name,
                    fdw_type,
                    options,
                },
            );
        }
        for row in catalog.load_foreign_tables()? {
            let columns: Vec<uqa_sql::ast::ColumnDef> =
                serde_json::from_str(&row.columns_json).unwrap_or_default();
            let options: BTreeMap<String, String> =
                serde_json::from_str(&row.options_json).unwrap_or_default();
            let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
                .iter()
                .map(|c| uqa_fdw::ColumnDef {
                    name: c.name.clone(),
                    ty: match &c.ty {
                        uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
                        uqa_sql::ast::ColumnType::Real
                        | uqa_sql::ast::ColumnType::Numeric { .. } => uqa_fdw::ColumnType::Real,
                        uqa_sql::ast::ColumnType::Text
                        | uqa_sql::ast::ColumnType::Json
                        | uqa_sql::ast::ColumnType::Date
                        | uqa_sql::ast::ColumnType::Time
                        | uqa_sql::ast::ColumnType::TimeTz
                        | uqa_sql::ast::ColumnType::Timestamp
                        | uqa_sql::ast::ColumnType::TimestampTz => uqa_fdw::ColumnType::Text,
                        uqa_sql::ast::ColumnType::Bytea
                        | uqa_sql::ast::ColumnType::Vector(_)
                        | uqa_sql::ast::ColumnType::Tensor(_) => uqa_fdw::ColumnType::Bytes,
                    },
                })
                .collect();
            self.foreign_tables.write().insert(
                row.name.clone(),
                uqa_fdw::ForeignTable {
                    name: row.name,
                    server_name: row.server_name,
                    columns: fdw_columns,
                    options,
                },
            );
        }
        Ok(())
    }

    fn restore_catalog_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for row in catalog.load_catalog_indexes()? {
            if !self.has_table(&row.table_name) {
                continue;
            }
            self.catalog_indexes
                .write()
                .insert(row.name.clone(), row.clone());
            let columns: Vec<String> = serde_json::from_str(&row.columns_json).unwrap_or_default();
            let parameters: BTreeMap<String, String> =
                serde_json::from_str(&row.parameters_json).unwrap_or_default();
            if row.index_type.eq_ignore_ascii_case("gin") {
                let analyzer = parameters
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                for col in &columns {
                    let _ = self.restore_fts_field_from_catalog(&row.table_name, col, analyzer);
                }
                if catalog.fts_storage_was_reset() {
                    if let Some(table) = self.table(&row.table_name) {
                        Self::rebuild_fts_index(&table).map_err(StorageBackendError::Other)?;
                    }
                }
            } else if row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw")
            {
                let params = IVFIndexParams::from_map_lossy(&parameters);
                for col in &columns {
                    if let Some(
                        uqa_sql::ast::ColumnType::Vector(dim)
                        | uqa_sql::ast::ColumnType::Tensor(dim),
                    ) = self.column_type(&row.table_name, col)
                    {
                        let _ = self.restore_ivf_vector_field(&row.table_name, col, dim, params);
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_path_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (key, seq_json) in catalog.load_path_indexes()? {
            let label_sequences: Vec<Vec<String>> =
                serde_json::from_str(&seq_json).unwrap_or_default();
            if let Some((graph, _name)) = key.split_once("::") {
                let graphs = self.graphs.read();
                let Some(store) = graphs.get(graph) else {
                    continue;
                };
                let idx = uqa_graph::PathIndex::build(store, graph, &label_sequences);
                drop(graphs);
                self.path_indexes.write().insert(key.clone(), idx);
            }
        }
        Ok(())
    }

    fn restore_graphs_from_catalog(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        use std::collections::BTreeMap;
        use uqa_graph::GraphStore as _;
        // Step 1: register every named graph (the registry table is
        // authoritative for empty graphs).
        let names = catalog.load_named_graphs()?;
        let mut graphs = self.graphs.write();
        for name in &names {
            graphs.entry(name.clone()).or_default();
            if let Some(store) = graphs.get_mut(name) {
                if !store.has_graph(name) {
                    store.create_graph(name);
                }
            }
        }
        // Step 2: load every vertex / edge into a side-table keyed
        // by global id. Memberships drive which graphs each entity
        // ends up attached to.
        let vertex_rows = catalog.load_vertices()?;
        let mut vertex_by_id: BTreeMap<u64, uqa_core::Vertex> = BTreeMap::new();
        for (id, label, props_json) in vertex_rows {
            let properties: BTreeMap<String, uqa_core::Value> = serde_json::from_str(&props_json)?;
            vertex_by_id.insert(
                id,
                uqa_core::Vertex {
                    vertex_id: id,
                    label,
                    properties,
                },
            );
        }
        let edge_rows = catalog.load_edges()?;
        let mut edge_by_id: BTreeMap<u64, uqa_core::Edge> = BTreeMap::new();
        for row in edge_rows {
            let properties: BTreeMap<String, uqa_core::Value> =
                serde_json::from_str(&row.properties_json)?;
            edge_by_id.insert(
                row.edge_id,
                uqa_core::Edge {
                    edge_id: row.edge_id,
                    source_id: row.source_id,
                    target_id: row.target_id,
                    label: row.label,
                    properties,
                },
            );
        }
        // Step 3: replay each membership row through the per-graph
        // store. add_vertex / add_edge populate the partition's
        // adjacency indexes for free.
        for (entity_type, entity_id, graph_name) in catalog.load_graph_memberships()? {
            let store = graphs.entry(graph_name.clone()).or_default();
            if !store.has_graph(&graph_name) {
                store.create_graph(&graph_name);
            }
            match entity_type.as_str() {
                "vertex" => {
                    if let Some(v) = vertex_by_id.get(&entity_id) {
                        store.add_vertex(v.clone(), &graph_name);
                    }
                }
                "edge" => {
                    if let Some(e) = edge_by_id.get(&entity_id) {
                        store.add_edge(e.clone(), &graph_name);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn is_persistent(&self) -> bool {
        self.catalog.is_some()
    }

    fn save_table_schema(&self, name: &str, table: &TableState) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let Ok(analyzer_json) = serde_json::to_string(&*table.analyzer.read()) else {
            return;
        };
        let vector_fields: Vec<VectorFieldSchema> = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, idx)| VectorFieldSchema {
                field: field.clone(),
                dimensions: idx.dimensions(),
            })
            .collect();
        let columns_json = serde_json::to_string(&*table.columns.read()).unwrap_or_default();
        let _ = catalog.save_table(&TableSchema {
            name: name.to_string(),
            analyzer_json,
            fts_fields: table.fts_fields(),
            vector_fields,
            columns_json,
        });
    }

    /// Register a table. `fts_fields` is the list of field names that are
    /// tokenized into the inverted index when documents are inserted.
    /// Other fields are still stored in the document store but are not
    /// queryable via `text_match` / [`TermOperator`].
    pub fn create_table(
        &self,
        name: impl Into<String>,
        analyzer: Analyzer,
        fts_fields: Vec<FieldName>,
    ) {
        let name = self.relation_name_for_create(&name.into());
        let (docs, inv): (Box<dyn DocumentStore>, Box<dyn InvertedIndex>) =
            if let Some(backend) = self.backend.as_ref() {
                (
                    backend.document_store(&name),
                    backend.inverted_index(&name, analyzer.clone()),
                )
            } else {
                (
                    Box::new(MemoryDocumentStore::new()),
                    Box::new(MemoryInvertedIndex::new(analyzer.clone())),
                )
            };
        let table = TableState {
            document_store: RwLock::new(docs),
            inverted_index: RwLock::new(inv),
            vector_indexes: RwLock::new(BTreeMap::new()),
            fts_fields: RwLock::new(fts_fields),
            columns: RwLock::new(Vec::new()),
            next_id: parking_lot::Mutex::new(1),
            analyzer: RwLock::new(analyzer),
            column_stats: RwLock::new(BTreeMap::new()),
            column_stats_loaded: AtomicBool::new(true),
            column_stats_dirty: AtomicBool::new(true),
            table_checks: RwLock::new(Vec::new()),
            foreign_keys: RwLock::new(Vec::new()),
        };
        let table_arc = Arc::new(table);
        self.tables.write().insert(name.clone(), table_arc.clone());
        if self.is_persistent() {
            self.save_table_schema(&name, &table_arc);
        }
    }

    /// Register an IVF vector field on a table. Existing document values
    /// in the same field are indexed immediately; later calls to
    /// [`Engine::add_vector`] or [`Engine::add_document_with_vectors`] keep
    /// it current.
    pub fn create_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> bool {
        self.install_vector_field(
            table,
            field.into(),
            dimensions,
            IVFIndexParams::default(),
            false,
            true,
        )
    }

    pub(crate) fn rebuild_ivf_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> bool {
        self.install_vector_field(table, field.into(), dimensions, params, true, true)
    }

    fn restore_ivf_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let field = field.into();
        let idx = self.build_vector_index_for_restore(table, &field, dimensions, params);
        t.vector_indexes.write().insert(field, idx);
        true
    }

    fn restore_fts_field_from_catalog(
        &self,
        table: &str,
        field: &str,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let t = self
            .table(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("restore_fts_field: {e}"))?;
            self.table_field_analyzers.write().insert(
                (table.to_string(), field.to_string()),
                (analyzer_name.to_string(), "both".to_string()),
            );
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.iter().any(|f| f == field) {
                fts.push(field.to_string());
            }
        }
        Ok(())
    }

    fn install_vector_field(
        &self,
        table: &str,
        field: FieldName,
        dimensions: u32,
        params: IVFIndexParams,
        replace_existing: bool,
        persist_schema: bool,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        if !replace_existing {
            if let Some(existing) = t.vector_indexes.read().get(&field) {
                return existing.dimensions() == dimensions;
            }
        }
        let mut idx = self.build_vector_index(table, &field, dimensions, params);
        Self::backfill_vector_index(&t, &field, idx.as_mut());
        t.vector_indexes.write().insert(field, idx);
        if persist_schema && self.is_persistent() {
            self.save_table_schema(table, &t);
        }
        true
    }

    fn build_vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> Box<dyn VectorIndex> {
        self.build_vector_index_with_initialize(table, field, dimensions, params, true)
    }

    fn build_vector_index_for_restore(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> Box<dyn VectorIndex> {
        self.build_vector_index_with_initialize(table, field, dimensions, params, false)
    }

    fn build_vector_index_with_initialize(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: IVFIndexParams,
        initialize: bool,
    ) -> Box<dyn VectorIndex> {
        if let Some(backend) = self.backend.as_ref() {
            backend.vector_index(
                table,
                field,
                dimensions,
                Some(PersistentVectorIndexParams {
                    nlist: params.nlist,
                    nprobe: params.nprobe,
                    train_threshold: params.train_threshold,
                    initialize,
                }),
            )
        } else {
            Box::new(IVFIndex::with_params(
                dimensions,
                params.nlist,
                params.nprobe,
                params.train_threshold,
            ))
        }
    }

    fn backfill_vector_index(table: &TableState, field: &str, idx: &mut dyn VectorIndex) {
        let docs = table.document_store.read().snapshot();
        for (doc_id, document) in docs.iter_all() {
            let Some(value) = document.get(field) else {
                continue;
            };
            if let Some(vectors) = Self::field_index_vectors(table, field, value) {
                idx.add_many(doc_id, vectors);
            }
        }
    }

    pub fn add_vector(&self, table: &str, doc_id: DocId, field: &str, vector: Vec<f32>) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return false;
        };
        idx.as_mut().add(doc_id, vector);
        true
    }

    pub fn add_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return false;
        };
        idx.as_mut().add_many(doc_id, vectors);
        true
    }

    pub fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<f32>>,
    ) {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.add_document_with_vector_values(table, doc_id, document, vector_values);
    }

    pub fn add_document_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
    ) {
        self.add_document(table, doc_id, document);
        for (field, vectors) in vectors {
            self.add_vector_values(table, doc_id, &field, vectors);
        }
    }

    pub fn create_default_table(&self, name: impl Into<String>, fts_fields: Vec<FieldName>) {
        self.create_table(name, standard_analyzer("english"), fts_fields);
    }

    /// Create a named in-memory graph. No-op if it already exists.
    pub fn create_graph(&self, name: impl Into<String>) {
        let name = name.into();
        let mut graphs = self.graphs.write();
        graphs.entry(name.clone()).or_default();
        drop(graphs);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(&name);
        }
    }

    /// Drop a named graph. No-op when the graph is missing.
    pub fn drop_graph(&self, name: &str) {
        self.graphs.write().remove(name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_named_graph(name);
            // Vertex / edge rows survive in the global tables until
            // every graph has detached them; sweep the orphans now so
            // the catalog stays in sync with the in-memory view.
            let _ = catalog.purge_orphan_graph_entities();
        }
    }

    /// Sorted list of every named graph registered on this engine.
    /// Mirrors the canonical UQA implementation's `Engine.list_graphs`.
    pub fn list_graphs(&self) -> Vec<String> {
        self.graphs.read().keys().cloned().collect()
    }

    /// Return `true` when a graph with `name` is registered.
    /// Mirrors the canonical UQA implementation's `Engine.has_graph`.
    pub fn has_graph(&self, name: &str) -> bool {
        self.graphs.read().contains_key(name)
    }

    /// Insert a vertex into a named graph. Auto-creates the graph if
    /// missing. Mirrors the canonical UQA implementation's `Engine.add_graph_vertex`.
    pub fn add_graph_vertex(&self, vertex: uqa_core::Vertex, graph: &str) {
        use uqa_graph::GraphStore as _;
        let vertex_id = vertex.vertex_id;
        // Snapshot the persistable shape (label + properties JSON)
        // before moving the value into the in-memory store so the
        // catalog write below sees the exact same data.
        let persist = self.catalog.as_ref().and_then(|_| {
            serde_json::to_string(&vertex.properties)
                .ok()
                .map(|p| (vertex.label.clone(), p))
        });
        {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            store.add_vertex(vertex, graph);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            if let Some((label, props_json)) = persist {
                let _ = catalog.save_vertex(vertex_id, &label, &props_json);
                let _ = catalog.save_graph_membership("vertex", vertex_id, graph);
            }
        }
    }

    /// Insert an edge into a named graph. Auto-creates the graph if
    /// missing. Mirrors the canonical UQA implementation's `Engine.add_graph_edge`.
    pub fn add_graph_edge(&self, edge: uqa_core::Edge, graph: &str) {
        use uqa_graph::GraphStore as _;
        let edge_id = edge.edge_id;
        let edge_source = edge.source_id;
        let edge_target = edge.target_id;
        let persist = self.catalog.as_ref().and_then(|_| {
            serde_json::to_string(&edge.properties)
                .ok()
                .map(|p| (edge.label.clone(), p))
        });
        {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            store.add_edge(edge, graph);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            if let Some((label, props_json)) = persist {
                let _ = catalog.save_edge(edge_id, edge_source, edge_target, &label, &props_json);
                let _ = catalog.save_graph_membership("edge", edge_id, graph);
            }
        }
    }

    /// Apply a [`uqa_graph::GraphDelta`] to a named graph as a single
    /// atomic batch of `add/remove vertex/edge` ops. Mirrors the canonical UQA implementation's
    /// `Engine.apply_graph_delta`.
    pub fn apply_graph_delta(&self, graph: &str, delta: &uqa_graph::GraphDelta) {
        use uqa_graph::DeltaOp;
        use uqa_graph::GraphStore as _;
        let mut graphs = self.graphs.write();
        let store = graphs.entry(graph.to_string()).or_default();
        if !store.has_graph(graph) {
            store.create_graph(graph);
        }
        for op in delta.ops() {
            match op {
                DeltaOp::AddVertex(v) => store.add_vertex(v.clone(), graph),
                DeltaOp::RemoveVertex(id) => store.remove_vertex(*id, graph),
                DeltaOp::AddEdge(e) => store.add_edge(e.clone(), graph),
                DeltaOp::RemoveEdge(id) => store.remove_edge(*id, graph),
            }
        }
        drop(graphs);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            let mut needs_purge = false;
            for op in delta.ops() {
                match op {
                    DeltaOp::AddVertex(v) => {
                        if let Ok(props_json) = serde_json::to_string(&v.properties) {
                            let _ = catalog.save_vertex(v.vertex_id, &v.label, &props_json);
                            let _ = catalog.save_graph_membership("vertex", v.vertex_id, graph);
                        }
                    }
                    DeltaOp::RemoveVertex(id) => {
                        let _ = catalog.delete_graph_membership("vertex", *id, graph);
                        needs_purge = true;
                    }
                    DeltaOp::AddEdge(e) => {
                        if let Ok(props_json) = serde_json::to_string(&e.properties) {
                            let _ = catalog.save_edge(
                                e.edge_id,
                                e.source_id,
                                e.target_id,
                                &e.label,
                                &props_json,
                            );
                            let _ = catalog.save_graph_membership("edge", e.edge_id, graph);
                        }
                    }
                    DeltaOp::RemoveEdge(id) => {
                        let _ = catalog.delete_graph_membership("edge", *id, graph);
                        needs_purge = true;
                    }
                }
            }
            if needs_purge {
                // Vertex / edge rows survive only while at least one
                // graph still references them via `_graph_membership`.
                let _ = catalog.purge_orphan_graph_entities();
            }
        }
        // Invalidate any cached path indexes for this graph: a path
        // index is built against a snapshot, so the safe move is to
        // drop them and let the caller rebuild on demand.
        self.path_indexes
            .write()
            .retain(|key, _| !key.starts_with(&format!("{graph}::")));
    }

    /// Build (or replace) a path index for `graph` keyed by `name`.
    /// `label_sequences` is the set of label sequences to materialise;
    /// each sequence becomes a hash-friendly direct lookup for RPQ.
    /// Mirrors the canonical UQA implementation's `Engine.build_path_index`.
    pub fn build_path_index(&self, name: &str, graph: &str, label_sequences: &[Vec<String>]) {
        let key = format!("{graph}::{name}");
        let idx = {
            let graphs = self.graphs.read();
            let Some(store) = graphs.get(graph) else {
                return;
            };
            uqa_graph::PathIndex::build(store, graph, label_sequences)
        };
        self.path_indexes.write().insert(key.clone(), idx);
        if let Some(catalog) = self.catalog.as_ref() {
            let seq_json = serde_json::to_string(label_sequences).unwrap_or_else(|_| "[]".into());
            let _ = catalog.save_path_index(&key, &seq_json);
        }
    }

    /// Drop a path index by `(graph, name)`. Returns `true` when an
    /// index was removed. Mirrors the canonical UQA implementation's `Engine.drop_path_index`.
    pub fn drop_path_index(&self, name: &str, graph: &str) -> bool {
        let key = format!("{graph}::{name}");
        let removed = self.path_indexes.write().remove(&key).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_path_index(&key);
            }
        }
        removed
    }

    /// Look up a path index by `(graph, name)`. Returns a clone so the
    /// caller is not tied to the engine's lock. Mirrors the canonical UQA implementation's
    /// `Engine.get_path_index`.
    pub fn get_path_index(&self, name: &str, graph: &str) -> Option<uqa_graph::PathIndex> {
        let key = format!("{graph}::{name}");
        self.path_indexes.read().get(&key).cloned()
    }

    /// Sorted list of registered path index keys. Each key has the
    /// shape `<graph>::<name>` so the caller can split as needed.
    pub fn list_path_indexes(&self) -> Vec<String> {
        self.path_indexes.read().keys().cloned().collect()
    }

    /// Read-only borrow of a named graph for ad-hoc query construction
    /// outside the SQL function path. Returns `None` when the graph
    /// is unknown.
    pub fn graph_with<R>(
        &self,
        name: &str,
        f: impl FnOnce(&uqa_graph::MemoryGraphStore) -> R,
    ) -> Option<R> {
        let graphs = self.graphs.read();
        graphs.get(name).map(f)
    }

    /// Mutable borrow of a named graph for vertex / edge insertion.
    pub fn graph_with_mut<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::MemoryGraphStore) -> R,
    ) -> Option<R> {
        let result = {
            let mut graphs = self.graphs.write();
            graphs.get_mut(name).map(f)
        };
        if result.is_some() {
            self.resync_graph_to_catalog(name);
        }
        result
    }

    /// Run a Cypher query against a named graph and return the
    /// `(columns, rows)` projected by the query's `RETURN` clause (or
    /// empty vectors when the query has no `RETURN`).
    ///
    /// This wires the full `CREATE` / `MERGE` / `SET` / `DELETE` /
    /// `UNWIND` surface through to the in-memory store. The graph is
    /// auto-created on first use, mirroring the canonical UQA implementation's
    /// `CypherCompiler.execute` behaviour.
    pub fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        use uqa_graph::cypher::{parse_cypher, CypherWriter};
        use uqa_graph::GraphStore as _;
        let q = parse_cypher(query)?;
        let result = {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            // Ensure the named partition exists inside the store as
            // well. The outer map only owns the store; create_graph
            // populates the store's own partition registry that
            // mutations key off of.
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            let mut writer = CypherWriter::new(store, graph).with_params(params);
            writer.execute(&q)?
        };
        self.resync_graph_to_catalog(graph);
        Ok(result)
    }

    /// Mirror the in-memory graph back to the catalog after a write.
    /// Cypher / `graph_with_mut` callers can edit the store directly,
    /// so the simplest correct strategy is a full resync of `graph`'s
    /// membership rows: drop every membership for the graph, re-insert
    /// each vertex / edge currently in the partition, then garbage
    /// collect any vertex / edge that fell out of every other graph's
    /// membership too.
    fn resync_graph_to_catalog(&self, graph: &str) {
        use uqa_graph::GraphStore as _;
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let graphs = self.graphs.read();
        let Some(store) = graphs.get(graph) else {
            return;
        };
        let _ = catalog.save_named_graph(graph);
        let _ = catalog.delete_graph_membership_for_graph(graph);
        for vertex in store.vertices_in_graph(graph) {
            if let Ok(props_json) = serde_json::to_string(&vertex.properties) {
                let _ = catalog.save_vertex(vertex.vertex_id, &vertex.label, &props_json);
                let _ = catalog.save_graph_membership("vertex", vertex.vertex_id, graph);
            }
        }
        for edge in store.edges_in_graph(graph) {
            if let Ok(props_json) = serde_json::to_string(&edge.properties) {
                let _ = catalog.save_edge(
                    edge.edge_id,
                    edge.source_id,
                    edge.target_id,
                    &edge.label,
                    &props_json,
                );
                let _ = catalog.save_graph_membership("edge", edge.edge_id, graph);
            }
        }
        let _ = catalog.purge_orphan_graph_entities();
    }

    /// Persist a deep-fusion model under `name`. Round-trips as JSON
    /// through the catalog's `_models` table when the engine is in
    /// `SQLite` mode; in-memory engines keep the latest version per
    /// process.
    pub fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        let json = serde_json::to_string(model)
            .map_err(|e| SQLError::Internal(format!("model serialise: {e}")))?;
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_model(name, &json)
                .map_err(|e| SQLError::Internal(format!("catalog save_model: {e}")))?;
        }
        self.models.write().insert(name.to_string(), model.clone());
        Ok(())
    }

    pub fn load_model(&self, name: &str) -> Option<DeepModel> {
        if let Some(m) = self.models.read().get(name).cloned() {
            return Some(m);
        }
        let catalog = self.catalog.as_ref()?;
        let json = catalog.load_model(name).ok().flatten()?;
        let model: DeepModel = serde_json::from_str(&json).ok()?;
        self.models.write().insert(name.to_string(), model.clone());
        Some(model)
    }

    pub fn drop_model(&self, name: &str) {
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_model(name);
        }
        self.models.write().remove(name);
    }

    /// compatibility alias for [`Engine::drop_model`].
    pub fn delete_model(&self, name: &str) {
        self.drop_model(name);
    }

    /// Train an analytical deep model and persist it under `name`.
    pub fn deep_learn(
        &self,
        name: &str,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let output = ml_deep_learn(training_set, options)
            .map_err(|e| SQLError::Unsupported(format!("deep_learn: {e}")))?;
        self.save_model(name, &output.model)?;
        Ok(output)
    }

    /// Parse a JSON [`TrainingSet`], train it, and persist the model.
    pub fn deep_learn_json(
        &self,
        name: &str,
        training_json: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set: TrainingSet = serde_json::from_str(training_json).map_err(|e| {
            SQLError::TypeMismatch(format!("invalid deep_learn training JSON: {e}"))
        })?;
        self.deep_learn(name, &training_set, options)
    }

    /// Train from a table containing `features` and `label` columns.
    pub fn deep_learn_table(
        &self,
        name: &str,
        table: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set = self.training_set_from_table(table, "features", "label")?;
        self.deep_learn(name, &training_set, options)
    }

    /// Persist Bayesian calibration parameters for a named signal. The
    /// parameters arrive serialised as a JSON string so callers can
    /// stuff arbitrary `(alpha, beta, base_rate, ...)` shapes through
    /// without forcing a struct. Mirrors the canonical UQA implementation's `save_scoring_params`.
    pub fn save_scoring_params(&self, name: &str, params_json: &str) -> Result<(), SQLError> {
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_scoring_params(name, params_json)
                .map_err(|e| SQLError::Internal(format!("catalog save_scoring_params: {e}")))?;
        }
        self.scoring_params
            .write()
            .insert(name.to_string(), params_json.to_string());
        Ok(())
    }

    /// Load persisted scoring parameters for a single signal. Falls
    /// back to the in-memory cache when the engine is not catalog-
    /// backed. Mirrors the canonical UQA implementation's `Engine.load_scoring_params`.
    pub fn load_scoring_params(&self, name: &str) -> Option<String> {
        if let Some(p) = self.scoring_params.read().get(name).cloned() {
            return Some(p);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            if let Ok(Some(json)) = catalog.load_scoring_params(name) {
                self.scoring_params
                    .write()
                    .insert(name.to_string(), json.clone());
                return Some(json);
            }
        }
        None
    }

    /// Snapshot every persisted `(name, params_json)` pair. Mirrors
    /// the canonical UQA implementation's `Engine.load_all_scoring_params`.
    pub fn load_all_scoring_params(&self) -> Vec<(String, String)> {
        if let Some(catalog) = self.catalog.as_ref() {
            if let Ok(rows) = catalog.load_all_scoring_params() {
                let mut cache = self.scoring_params.write();
                for (name, json) in &rows {
                    cache.insert(name.clone(), json.clone());
                }
                return rows;
            }
        }
        let map = self.scoring_params.read();
        let mut out: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Drop persisted scoring parameters for a single signal. Returns
    /// `true` when something was removed.
    pub fn drop_scoring_params(&self, name: &str) -> bool {
        let mut removed = self.scoring_params.write().remove(name).is_some();
        if let Some(catalog) = self.catalog.as_ref() {
            removed = catalog.drop_scoring_params(name).is_ok() || removed;
        }
        removed
    }

    /// Run inference for a saved model against a fresh execution
    /// context. Returns `(doc_id, score)` pairs ordered by `doc_id`.
    pub fn deep_predict(&self, name: &str) -> Option<Vec<(DocId, f64)>> {
        let model = self.load_model(name)?;
        let ctx = ExecutionContext::new();
        let (scores, _) = model.predict(&ctx);
        Some(scores)
    }

    pub fn deep_predict_features(
        &self,
        name: &str,
        examples: &[(DocId, Vec<f64>)],
    ) -> Result<Vec<(DocId, f64)>, SQLError> {
        let model = self
            .load_model(name)
            .ok_or_else(|| SQLError::Unsupported(format!("unknown model {name:?}")))?;
        let (scores, _) = model
            .predict_features(examples)
            .map_err(|e| SQLError::Unsupported(format!("deep_predict: {e}")))?;
        Ok(scores)
    }

    fn relation_lookup_candidates(&self, name: &str) -> Vec<String> {
        if name.contains('.') {
            return vec![name.to_string()];
        }
        let mut candidates = Vec::new();
        for schema in self.search_path.read().iter() {
            if schema == "pg_catalog" || schema == "information_schema" {
                continue;
            }
            if schema == "public" {
                candidates.push(name.to_string());
            } else {
                candidates.push(format!("{schema}.{name}"));
            }
        }
        if !candidates.iter().any(|candidate| candidate == name) {
            candidates.push(name.to_string());
        }
        candidates
    }

    fn relation_name_for_create(&self, name: &str) -> String {
        if name.contains('.') {
            return name.to_string();
        }
        let schema = self
            .search_path
            .read()
            .iter()
            .find(|schema| {
                schema.as_str() != "pg_catalog" && schema.as_str() != "information_schema"
            })
            .cloned()
            .unwrap_or_else(|| "public".to_string());
        if schema == "public" {
            name.to_string()
        } else {
            format!("{schema}.{name}")
        }
    }

    pub(crate) fn resolve_table_name(&self, name: &str) -> Option<String> {
        let tables = self.tables.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
    }

    fn resolve_view_name(&self, name: &str) -> Option<String> {
        let views = self.views.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| views.contains_key(candidate))
    }

    fn resolve_sequence_name(&self, name: &str) -> Option<String> {
        let sequences = self.sequences.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| sequences.contains_key(candidate))
    }

    fn resolve_foreign_table_name(&self, name: &str) -> Option<String> {
        let tables = self.foreign_tables.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
    }

    fn table(&self, name: &str) -> Option<Arc<TableState>> {
        let resolved = self.resolve_table_name(name)?;
        self.tables.read().get(&resolved).cloned()
    }

    fn training_set_from_table(
        &self,
        table: &str,
        features_field: &str,
        label_field: &str,
    ) -> Result<TrainingSet, SQLError> {
        let table_state = self
            .table(table)
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let store = table_state.document_store.read();
        let mut examples = Vec::new();
        for (doc_id, document) in store.iter_all() {
            let features = document.get(features_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{features_field}`"
                ))
            })?;
            let label = document.get(label_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{label_field}`"
                ))
            })?;
            examples.push(TrainingExample {
                features: value_to_f64_vec(features).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{features_field}`: {e}"
                    ))
                })?,
                label: value_to_usize(label).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{label_field}`: {e}"
                    ))
                })?,
            });
        }
        Ok(TrainingSet {
            examples,
            class_count: None,
        })
    }

    pub(crate) fn fts_fields_for_table(&self, name: &str) -> Vec<FieldName> {
        self.table(name)
            .map_or_else(Vec::new, |table| table.fts_fields())
    }

    pub fn fts_index_stats(&self, table_filter: Option<&str>) -> Vec<FtsIndexStat> {
        let mut tables: Vec<(String, Arc<TableState>)> = self
            .tables
            .read()
            .iter()
            .filter(|(name, _)| table_filter.is_none_or(|target| name.as_str() == target))
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect();
        tables.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        for (table_name, table) in tables {
            let mut fields = table.fts_fields();
            fields.sort();
            let index = table.inverted_index.read();
            for field in fields {
                let analyzer = self.table_field_analyzer(&table_name, &field).map_or_else(
                    || analyzer_registry::DEFAULT_ANALYZER_NAME.to_string(),
                    |(name, _)| name,
                );
                let doc_length_count = index.doc_length_count(Some(&field));
                out.push(FtsIndexStat {
                    table_name: table_name.clone(),
                    field: field.clone(),
                    analyzer,
                    posting_count: index.posting_count(Some(&field)),
                    doc_length_count,
                    indexed_doc_count: doc_length_count,
                    term_count: index.term_count(Some(&field)),
                    total_field_length: index.total_field_length(&field),
                });
            }
        }
        out
    }

    fn rebuild_fts_index(t: &Arc<TableState>) -> Result<(), String> {
        let fts_fields = t.fts_fields();
        let docs: Vec<(DocId, Document)> = {
            let store = t.document_store.read();
            store.iter_all().collect()
        };
        let mut index = t.inverted_index.write();
        index.try_clear()?;
        for (doc_id, document) in docs {
            let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
            for field in &fts_fields {
                if let Some(Value::Str(s)) = document.get(field) {
                    text_fields.insert(field.clone(), s.clone());
                }
            }
            if !text_fields.is_empty() {
                index.try_add_document(doc_id, text_fields)?;
            }
        }
        Ok(())
    }

    pub fn add_document(&self, table: &str, doc_id: DocId, document: Document) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        // Index the FTS fields whose values are strings.
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for name in &t.fts_fields() {
            if let Some(Value::Str(s)) = document.get(name) {
                text_fields.insert(name.clone(), s.clone());
            }
        }
        if !text_fields.is_empty() {
            t.inverted_index.write().add_document(doc_id, text_fields);
        }
        t.document_store.write().put(doc_id, document);
        self.mark_column_stats_dirty(&table_name, &t);
        // Keep the auto-id watermark monotonic over manual inserts as well.
        let mut nx = t.next_id.lock();
        if doc_id >= *nx {
            *nx = doc_id + 1;
        }
    }

    /// Register a view body. The engine treats views as named
    /// `SELECT` aliases that re-materialise on every reference; the
    /// body is stored verbatim and resolved by the SQL surface
    /// whenever a query references the view name.
    pub fn register_view(&self, name: &str, body: uqa_sql::ast::SelectStmt) {
        let name = self.relation_name_for_create(name);
        self.views.write().insert(name.clone(), body);
        self.persist_views();
    }

    pub fn drop_view(&self, name: &str) -> bool {
        let Some(name) = self.resolve_view_name(name) else {
            return false;
        };
        let removed = self.views.write().remove(&name).is_some();
        if removed {
            self.persist_views();
        }
        removed
    }

    pub fn view(&self, name: &str) -> Option<uqa_sql::ast::SelectStmt> {
        let resolved = self.resolve_view_name(name)?;
        self.views.read().get(&resolved).cloned()
    }

    pub fn list_views(&self) -> Vec<String> {
        let mut out: Vec<String> = self.views.read().keys().cloned().collect();
        out.sort_unstable();
        out
    }

    fn persist_views(&self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        if let Ok(json) = serde_json::to_string(&*self.views.read()) {
            let _ = catalog.set_metadata(VIEWS_METADATA_KEY, &json);
        }
    }

    fn restore_views_from_metadata(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(VIEWS_METADATA_KEY)? else {
            return Ok(());
        };
        if let Ok(views) = serde_json::from_str::<BTreeMap<String, uqa_sql::ast::SelectStmt>>(&json)
        {
            *self.views.write() = views;
        }
        Ok(())
    }

    pub fn list_catalog_indexes(&self) -> Vec<CatalogIndexRow> {
        let mut out: Vec<CatalogIndexRow> = self.catalog_indexes.read().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Register a schema name. Schemas in the engine map onto
    /// optional table prefixes; the registry just records the name
    /// so subsequent statements that reference it do not error out.
    pub fn register_schema(&self, name: &str, _if_not_exists: bool) {
        self.schemas.write().insert(name.to_string());
    }

    pub fn drop_schema(&self, name: &str) -> bool {
        self.schemas.write().remove(name)
    }

    /// Sorted list of every registered schema. Mirrors the canonical UQA implementation's
    /// `Engine._tables.schemas`.
    pub fn list_schemas(&self) -> Vec<String> {
        let mut out: Vec<String> = self.schemas.read().iter().cloned().collect();
        if !out.iter().any(|s| s == "public") {
            out.insert(0, "public".to_string());
        }
        out
    }

    /// Tables that belong to a schema. Names matching `<schema>.X`
    /// are bucketed under `<schema>`; everything else falls under
    /// `public`. Mirrors the canonical UQA implementation's `Engine._tables.tables_in_schema`.
    pub fn tables_in_schema(&self, schema: &str) -> Vec<String> {
        let prefix = format!("{schema}.");
        let mut out: Vec<String> = Vec::new();
        for name in self.tables.read().keys() {
            if let Some(rest) = name.strip_prefix(&prefix) {
                out.push(rest.to_string());
            } else if schema == "public" && !name.contains('.') {
                out.push(name.clone());
            }
        }
        out.sort_unstable();
        out
    }

    pub fn list_sequences(&self) -> Vec<String> {
        let mut out: Vec<String> = self.sequences.read().keys().cloned().collect();
        out.sort_unstable();
        out
    }

    /// Current `search_path`. Mirrors the canonical UQA implementation's
    /// `Engine._tables.search_path`.
    pub fn search_path(&self) -> Vec<String> {
        self.search_path.read().clone()
    }

    /// Replace the `search_path`. Empty input falls back to `["public"]`.
    pub fn set_search_path(&self, path: Vec<String>) {
        let mut value = path;
        if value.is_empty() {
            value.push("public".to_string());
        }
        *self.search_path.write() = value;
    }

    /// Apply `SET <name> [TO|=] <value>`. Honours `search_path`
    /// directly; every other parameter is stored in the session-vars
    /// map so a subsequent `SHOW <name>` can echo it back. Mirrors
    /// the canonical UQA implementation's session-variable behaviour.
    pub fn set_variable(&self, name: &str, value: &str) {
        if name.eq_ignore_ascii_case("search_path") {
            let parts: Vec<String> = value
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            self.set_search_path(parts);
            self.session_vars
                .write()
                .insert(name.to_string(), value.to_string());
            return;
        }
        self.session_vars
            .write()
            .insert(name.to_string(), value.to_string());
    }

    /// Read back a session variable. `search_path` always resolves to
    /// the current resolution order; every other key looks up the
    /// session-vars map, then PostgreSQL-compatible runtime defaults,
    /// and finally an empty string. Mirrors the canonical UQA
    /// implementation's `_compile_show`.
    pub fn show_variable(&self, name: &str) -> String {
        if name.eq_ignore_ascii_case("search_path") {
            return self.search_path().join(",");
        }
        let session_vars = self.session_vars.read();
        if let Some(value) = session_vars.get(name) {
            return value.clone();
        }
        if let Some((_, value)) = session_vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return value.clone();
        }
        default_runtime_parameter(name).unwrap_or("").to_string()
    }

    /// Apply `DISCARD <target>`. Mirrors the canonical UQA implementation's `_compile_discard`:
    /// `ALL` resets every kind of session state; the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) {
        use uqa_sql::ast::DiscardTarget;
        match target {
            DiscardTarget::All => {
                self.session_vars.write().clear();
                self.prepared.write().clear();
                self.set_search_path(vec!["public".to_string()]);
                // Temp tables aren't tracked separately yet; clearing
                // the prepared map matches the canonical UQA implementation's effect on the bits
                // we own today.
            }
            DiscardTarget::Plans => {
                self.prepared.write().clear();
            }
            DiscardTarget::Sequences => {
                self.sequences.write().clear();
            }
            DiscardTarget::Temp => {
                // No temp-table registry yet; preserve the no-op
                // semantics until we add one.
            }
        }
    }

    /// Refresh per-column statistics for a single table or every
    /// table when `table` is `None`. Mirrors `Table.analyze` in
    /// the canonical UQA behavior: scans every document, collects per-
    /// column distinct count / null count / min / max / equi-depth
    /// histogram (100 buckets) / MCV list (top 10 above-average
    /// frequency), and stores the result on the per-table state so the
    /// cardinality estimator can read it on subsequent queries.
    pub fn run_analyze(&self, table: Option<&str>) {
        let names: Vec<String> = match table {
            Some(t) => vec![t.to_string()],
            None => self.tables.read().keys().cloned().collect(),
        };
        for name in names {
            let Some(t) = self.table(&name) else { continue };
            self.analyze_table(&name, &t);
        }
    }

    fn mark_column_stats_dirty(&self, table_name: &str, table: &Arc<TableState>) {
        if !table.column_stats_dirty.swap(true, Ordering::AcqRel) {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.delete_column_stats(table_name);
            }
        }
    }

    fn analyze_table(&self, table_name: &str, t: &Arc<TableState>) {
        let snapshot = t.document_store.read().snapshot();
        let doc_ids: Vec<DocId> = {
            let mut v = snapshot.doc_ids();
            v.sort_unstable();
            v
        };
        let n = doc_ids.len() as u64;
        let columns: Vec<String> = t.columns.read().iter().map(|c| c.name.clone()).collect();

        let mut col_values: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut col_nulls: BTreeMap<String, u64> = BTreeMap::new();
        for col in &columns {
            col_values.insert(col.clone(), Vec::new());
            col_nulls.insert(col.clone(), 0);
        }

        for doc_id in &doc_ids {
            let Some(doc) = snapshot.get(*doc_id) else {
                for col in &columns {
                    *col_nulls.get_mut(col).unwrap() += 1;
                }
                continue;
            };
            for col in &columns {
                match doc.get(col) {
                    None | Some(Value::Null) => {
                        *col_nulls.get_mut(col).unwrap() += 1;
                    }
                    Some(v) => {
                        col_values.get_mut(col).unwrap().push(v.clone());
                    }
                }
            }
        }

        let mut stats_out: BTreeMap<String, uqa_planner::ColumnStats> = BTreeMap::new();
        for col in &columns {
            let values = col_values.remove(col).unwrap_or_default();
            let null_count = col_nulls.remove(col).unwrap_or(0);
            let distinct = distinct_count(&values);
            let comparable: Vec<&Value> = values
                .iter()
                .filter(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
                    )
                })
                .collect();
            let min_val = comparable.iter().min().map(|v| (*v).clone());
            let max_val = comparable.iter().max().map(|v| (*v).clone());

            let histogram = build_histogram(&comparable);
            let (mcv_values, mcv_frequencies) = build_mcv(&values, n);

            stats_out.insert(
                col.clone(),
                uqa_planner::ColumnStats {
                    distinct_count: distinct,
                    null_count,
                    min_value: min_val,
                    max_value: max_val,
                    row_count: n,
                    histogram,
                    mcv_values,
                    mcv_frequencies,
                },
            );
        }

        if let Some(catalog) = self.catalog.as_ref() {
            let _ = Self::persist_column_stats(catalog.as_ref(), table_name, &stats_out);
        }
        *t.column_stats.write() = stats_out;
        t.column_stats_loaded.store(true, Ordering::Release);
        t.column_stats_dirty.store(false, Ordering::Release);
    }

    fn persist_column_stats(
        catalog: &dyn CatalogFacade,
        table_name: &str,
        stats: &BTreeMap<String, uqa_planner::ColumnStats>,
    ) -> StorageBackendResult<()> {
        catalog.delete_column_stats(table_name)?;
        for (col_name, cs) in stats {
            let min_json = cs
                .min_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let max_json = cs
                .max_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let histogram_json = serde_json::to_string(&cs.histogram)?;
            let mcv_values_json = serde_json::to_string(&cs.mcv_values)?;
            let mcv_frequencies_json = serde_json::to_string(&cs.mcv_frequencies)?;
            catalog.save_column_stats(ColumnStatsInput {
                table_name,
                column_name: col_name,
                distinct_count: Self::u64_to_i64(cs.distinct_count),
                null_count: Self::u64_to_i64(cs.null_count),
                min_value: min_json.as_deref(),
                max_value: max_json.as_deref(),
                row_count: Self::u64_to_i64(cs.row_count),
                histogram_json: &histogram_json,
                mcv_values_json: &mcv_values_json,
                mcv_frequencies_json: &mcv_frequencies_json,
            })?;
        }
        Ok(())
    }

    fn u64_to_i64(n: u64) -> i64 {
        i64::try_from(n).unwrap_or(i64::MAX)
    }

    /// Snapshot of the cardinality estimator's per-column statistics
    /// for `table`. Dirty stats are recomputed lazily so callers do not
    /// need to issue `ANALYZE` after every data change.
    pub fn column_stats(&self, table: &str) -> BTreeMap<String, uqa_planner::ColumnStats> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        self.load_column_stats_if_needed(table, &t);
        if t.column_stats_dirty.load(Ordering::Acquire) {
            self.analyze_table(table, &t);
        }
        let stats = t.column_stats.read().clone();
        stats
    }

    fn load_column_stats_if_needed(&self, table: &str, t: &Arc<TableState>) {
        if t.column_stats_loaded.load(Ordering::Acquire) {
            return;
        }
        if t.column_stats_loaded
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let stats = self
            .catalog
            .as_ref()
            .and_then(|catalog| Self::load_column_stats_from_catalog(catalog.as_ref(), table).ok())
            .unwrap_or_default();
        let missing_stats = stats.is_empty() && !t.columns.read().is_empty();
        *t.column_stats.write() = stats;
        if missing_stats {
            t.column_stats_dirty.store(true, Ordering::Release);
        }
    }

    /// Wipe every row from the named table while keeping the schema
    /// (catalog row + analyzer + column list) intact. Mirrors
    /// `TRUNCATE TABLE`.
    pub fn truncate_table(&self, name: &str) {
        let Some(t) = self.table(name) else {
            return;
        };
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            t.document_store.write().delete(doc_id);
            t.inverted_index.write().remove_document(doc_id);
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id);
            }
        }
        *t.next_id.lock() = 1;
        self.mark_column_stats_dirty(name, &t);
    }

    /// Execute a transaction control statement (`BEGIN` / `COMMIT` /
    /// `ROLLBACK` / savepoint variants) against the engine. The
    /// engine maintains a single transaction stack per connection.
    /// Start an explicit transaction frame. Matches UQA behavior for
    /// `Engine.begin`. Equivalent to running `BEGIN` through `sql`.
    pub fn begin(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Begin)
    }

    /// Commit the top-most transaction frame. Matches UQA behavior for
    /// `Engine.commit`.
    pub fn commit(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Commit)
    }

    /// Roll back the top-most transaction frame. Matches UQA behavior for
    /// `Engine.rollback`.
    pub fn rollback(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Rollback)
    }

    /// Mark a savepoint inside the current transaction. Matches UQA behavior for
    /// `Engine.savepoint(name)`.
    pub fn savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::Savepoint(name.to_string()))
    }

    /// Release a savepoint. Matches UQA behavior for `Engine.release_savepoint`.
    pub fn release_savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::ReleaseSavepoint(
            name.to_string(),
        ))
    }

    /// Roll back to a named savepoint. Matches UQA behavior for
    /// `Engine.rollback_to_savepoint`.
    pub fn rollback_to_savepoint(&self, name: &str) -> Result<(), SQLError> {
        self.run_transaction_statement(uqa_sql::ast::TransactionStmt::RollbackToSavepoint(
            name.to_string(),
        ))
    }

    /// Run `f` inside one engine transaction. On success the transaction is
    /// committed; on error or panic it is rolled back before the error/panic is
    /// returned to the caller.
    pub fn transaction<R>(
        &self,
        f: impl FnOnce(&Self) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        self.begin()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.commit()?;
                Ok(value)
            }
            Ok(Err(err)) => {
                if let Err(rollback_err) = self.rollback() {
                    return Err(SQLError::Internal(format!(
                        "transaction rollback after error failed: {rollback_err}; original error: {err}"
                    )));
                }
                Err(err)
            }
            Err(payload) => {
                let _ = self.rollback();
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Execute multiple SQL statements inside one engine transaction.
    pub fn sql_batch(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<Vec<SQLResult>, SQLError> {
        self.transaction(|engine| {
            let mut results = Vec::with_capacity(statements.len());
            for (sql, params) in statements {
                results.push(engine.sql(sql, params)?);
            }
            Ok(results)
        })
    }

    /// Number of currently-open transaction frames (`BEGIN` count
    /// minus `COMMIT/ROLLBACK` count). Useful for assertions in tests
    /// and for status displays in the CLI.
    pub fn transaction_depth(&self) -> usize {
        self.tx_stack.lock().len()
    }

    /// Tear down engine state cleanly. Rolls back any open transaction
    /// frames and clears registries. Matches UQA behavior for `Engine.close`.
    /// The engine value can no longer be used afterwards in a
    /// well-defined sense; idiomatic Rust drops the value at scope
    /// exit, but this method exists for API compatibility
    /// reference and for explicit shutdown ordering.
    pub fn close(&self) {
        // Roll back every open transaction.
        let had_open_transaction = {
            let mut guard = self.tx_stack.lock();
            let had_open_transaction = !guard.is_empty();
            guard.clear();
            had_open_transaction
        };
        if had_open_transaction {
            if let Some(backend) = self.backend.as_ref() {
                let _ = backend.rollback_transaction();
            }
        }
        // Clear FDW registries - closing connections is up to the
        // handler, but dropping the catalog entries is enough to free
        // the registered handles.
        self.foreign_servers.write().clear();
        self.foreign_tables.write().clear();
        self.foreign_memory_tables.write().clear();
    }

    pub fn run_transaction_statement(
        &self,
        tx: uqa_sql::ast::TransactionStmt,
    ) -> Result<(), SQLError> {
        use uqa_sql::ast::TransactionStmt;
        let mut guard = self.tx_stack.lock();
        match tx {
            TransactionStmt::Begin => self.begin_transaction_frame(&mut guard)?,
            TransactionStmt::Commit => self.commit_transaction_frame(&mut guard)?,
            TransactionStmt::Rollback => self.rollback_transaction_frame(&mut guard)?,
            TransactionStmt::Savepoint(name) => {
                self.save_transaction_savepoint(&mut guard, name)?;
            }
            TransactionStmt::ReleaseSavepoint(name) => {
                self.release_transaction_savepoint(&mut guard, &name)?;
            }
            TransactionStmt::RollbackToSavepoint(name) => {
                self.rollback_to_transaction_savepoint(&mut guard, &name)?;
            }
        }
        Ok(())
    }

    fn begin_transaction_frame(&self, stack: &mut Vec<TransactionFrame>) -> Result<(), SQLError> {
        let storage_savepoint = if stack.is_empty() {
            if let Some(backend) = self.backend.as_ref() {
                backend
                    .begin_transaction()
                    .map_err(|err| Self::storage_tx_error("BEGIN", &err))?;
            }
            None
        } else {
            let savepoint = format!("__uqa_nested_tx_{}", stack.len());
            if let Some(backend) = self.backend.as_ref() {
                backend
                    .savepoint(&savepoint)
                    .map_err(|err| Self::storage_tx_error("nested BEGIN savepoint", &err))?;
            }
            Some(savepoint)
        };
        stack.push(TransactionFrame {
            storage_savepoint,
            savepoints: std::collections::BTreeSet::new(),
            data_snapshot: self.snapshot_transaction_data(),
            data_savepoints: BTreeMap::new(),
        });
        Ok(())
    }

    fn commit_transaction_frame(&self, stack: &mut Vec<TransactionFrame>) -> Result<(), SQLError> {
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?
            .storage_savepoint
            .clone();
        if let Some(backend) = self.backend.as_ref() {
            if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .release_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested COMMIT savepoint", &err))?;
            } else {
                backend
                    .commit_transaction()
                    .map_err(|err| Self::storage_tx_error("COMMIT", &err))?;
            }
        }
        stack.pop();
        Ok(())
    }

    fn rollback_transaction_frame(
        &self,
        stack: &mut Vec<TransactionFrame>,
    ) -> Result<(), SQLError> {
        let storage_savepoint = stack
            .last()
            .ok_or_else(|| SQLError::Internal("ROLLBACK without an open transaction".into()))?
            .storage_savepoint
            .clone();
        if let Some(backend) = self.backend.as_ref() {
            if let Some(savepoint) = storage_savepoint.as_ref() {
                backend
                    .rollback_to_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested ROLLBACK savepoint", &err))?;
                backend
                    .release_savepoint(savepoint)
                    .map_err(|err| Self::storage_tx_error("nested ROLLBACK release", &err))?;
            } else {
                backend
                    .rollback_transaction()
                    .map_err(|err| Self::storage_tx_error("ROLLBACK", &err))?;
            }
        }
        if let Some(snapshot) = stack.last().and_then(|frame| frame.data_snapshot.clone()) {
            self.restore_transaction_data(&snapshot);
        }
        stack.pop();
        Ok(())
    }

    fn save_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: String,
    ) -> Result<(), SQLError> {
        let frame = stack
            .last_mut()
            .ok_or_else(|| SQLError::Internal("SAVEPOINT outside a transaction".into()))?;
        if let Some(backend) = self.backend.as_ref() {
            backend
                .savepoint(&name)
                .map_err(|err| Self::storage_tx_error("SAVEPOINT", &err))?;
        }
        if let Some(snapshot) = self.snapshot_transaction_data() {
            frame.data_savepoints.insert(name.clone(), snapshot);
        }
        frame.savepoints.insert(name);
        Ok(())
    }

    fn release_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let frame = stack
            .last_mut()
            .ok_or_else(|| SQLError::Internal("RELEASE SAVEPOINT outside a transaction".into()))?;
        if let Some(backend) = self.backend.as_ref() {
            backend
                .release_savepoint(name)
                .map_err(|err| Self::storage_tx_error("RELEASE SAVEPOINT", &err))?;
        }
        frame.savepoints.remove(name);
        frame.data_savepoints.remove(name);
        Ok(())
    }

    fn rollback_to_transaction_savepoint(
        &self,
        stack: &mut [TransactionFrame],
        name: &str,
    ) -> Result<(), SQLError> {
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("ROLLBACK TO SAVEPOINT outside a transaction".into())
        })?;
        if !frame.savepoints.contains(name) {
            return Err(SQLError::Internal(format!("savepoint `{name}` not found")));
        }
        if let Some(backend) = self.backend.as_ref() {
            backend
                .rollback_to_savepoint(name)
                .map_err(|err| Self::storage_tx_error("ROLLBACK TO SAVEPOINT", &err))?;
        }
        if let Some(snapshot) = frame.data_savepoints.get(name).cloned() {
            self.restore_transaction_data(&snapshot);
        }
        Ok(())
    }

    fn storage_tx_error(action: &str, err: &StorageBackendError) -> SQLError {
        SQLError::Internal(format!("{action} failed in storage backend: {err}"))
    }

    fn snapshot_transaction_data(&self) -> Option<EngineDataSnapshot> {
        if self.backend.is_some() {
            return None;
        }
        let mut tables = BTreeMap::new();
        for (name, table) in self.tables.read().iter() {
            let documents = table.document_store.read().iter_all().collect();
            let next_id = *table.next_id.lock();
            tables.insert(
                name.clone(),
                TableDataSnapshot {
                    state: table.clone(),
                    documents,
                    next_id,
                },
            );
        }
        Some(EngineDataSnapshot {
            tables,
            sequences: self.sequences_snapshot(),
        })
    }

    fn restore_transaction_data(&self, snapshot: &EngineDataSnapshot) {
        {
            let mut tables = self.tables.write();
            tables.retain(|name, _| snapshot.tables.contains_key(name));
            for (name, table_snapshot) in &snapshot.tables {
                tables
                    .entry(name.clone())
                    .or_insert_with(|| table_snapshot.state.clone());
            }
        }
        for (name, table_snapshot) in &snapshot.tables {
            let Some(table) = self.table(name) else {
                continue;
            };
            table.document_store.write().clear();
            table.inverted_index.write().clear();
            for index in table.vector_indexes.write().values_mut() {
                index.clear();
            }
            for (doc_id, document) in &table_snapshot.documents {
                let vectors = Self::document_vector_values(&table, document);
                self.add_document_with_vector_values(name, *doc_id, document.clone(), vectors);
            }
            *table.next_id.lock() = table_snapshot.next_id;
        }
        *self.sequences.write() = snapshot.sequences.clone();
    }

    fn document_vector_values(
        table: &Arc<TableState>,
        document: &Document,
    ) -> BTreeMap<FieldName, Vec<Vec<f32>>> {
        let vector_fields: Vec<FieldName> = table.vector_indexes.read().keys().cloned().collect();
        let mut vectors = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(table, &field, value) {
                vectors.insert(field, values);
            }
        }
        vectors
    }

    fn field_index_vectors(
        table: &TableState,
        field: &str,
        value: &Value,
    ) -> Option<Vec<Vec<f32>>> {
        let ty = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == field)
            .map(|column| column.ty.clone());
        match ty {
            Some(uqa_sql::ast::ColumnType::Tensor(dim)) => {
                let tensor = uqa_sql::expr::value_to_tensor(value).ok()?;
                tensor
                    .iter()
                    .all(|vector| vector.len() as u32 == dim)
                    .then_some(tensor)
            }
            Some(uqa_sql::ast::ColumnType::Vector(dim)) => {
                let vector = uqa_sql::expr::value_to_vector(value).ok()?;
                (vector.len() as u32 == dim).then_some(vec![vector])
            }
            _ => uqa_sql::expr::value_to_vector(value)
                .ok()
                .map(|vector| vec![vector]),
        }
    }

    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> bool {
        let Some(name) = self.resolve_table_name(name) else {
            return false;
        };
        let removed = self.tables.write().remove(&name).is_some();
        if !removed {
            return false;
        }
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.table_field_analyzers
            .write()
            .retain(|(t, _), _| t != &name);
        self.catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(&name);
            let _ = catalog.purge_table_data(&name);
            let _ = catalog.drop_table_field_analyzers(&name);
            let _ = catalog.drop_catalog_indexes_for_table(&name);
        }
        true
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.resolve_table_name(name).is_some()
    }

    /// All schema-declared columns for `table`, in declaration order.
    pub fn table_columns(&self, table: &str) -> Vec<String> {
        self.table(table)
            .map(|t| t.columns.read().iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn table_has_column(&self, table: &str, column: &str) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let cols = t.columns.read();
        cols.iter().any(|c| c.name == column)
    }

    pub(crate) fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> Option<uqa_sql::ast::ColumnType> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter().find(|c| c.name == column).map(|c| c.ty.clone())
    }

    /// Return the SERIAL/BIGSERIAL column name for `table`, if any.
    pub(crate) fn auto_increment_column(&self, table: &str) -> Option<String> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter()
            .find(|c| c.auto_increment)
            .map(|c| c.name.clone())
    }

    /// Sorted list of every registered table name.
    pub fn table_names(&self) -> Vec<String> {
        self.tables.read().keys().cloned().collect()
    }

    /// Snapshot the column schema of `table`. Returns `None` when no
    /// table by that name is registered.
    pub fn describe_table(&self, table: &str) -> Option<Vec<uqa_sql::ast::ColumnDef>> {
        self.table(table).map(|t| t.columns.read().clone())
    }

    /// DEFAULT expression for `column` on `table`, when one was
    /// declared via `... <col> <type> DEFAULT <expr>`.
    pub fn column_default_expr(&self, table: &str, column: &str) -> Option<uqa_sql::ast::Expr> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter()
            .find(|c| c.name == column)
            .and_then(|c| c.default.clone())
    }

    pub fn set_column_default(
        &self,
        table: &str,
        column: &str,
        default: Option<uqa_sql::ast::Expr>,
    ) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            if let Some(col) = cols.iter_mut().find(|col| col.name == column) {
                col.default = default;
                found = true;
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    pub fn set_column_not_null(&self, table: &str, column: &str, not_null: bool) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            for col in cols.iter_mut() {
                if col.name == column {
                    col.not_null = not_null;
                    found = true;
                    break;
                }
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    pub fn set_column_type(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            if let Some(col) = cols.iter_mut().find(|col| col.name == column) {
                col.ty.clone_from(ty);
                found = true;
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    /// Register table-level CHECK + FK constraints. Called by the
    /// SQL `CREATE TABLE` path after the columns are in place.
    pub fn register_table_constraints(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
    ) {
        let Some(t) = self.table(table) else { return };
        *t.table_checks.write() = checks;
        *t.foreign_keys.write() = foreign_keys;
    }

    /// Snapshot of every CHECK constraint that applies to `table`,
    /// merging the column-level CHECKs into the table-level list.
    /// Returns `(name, expr)` pairs where `name` is the constraint
    /// name when one was supplied (synthesised as `<col>_check` for
    /// column-level constraints).
    pub fn check_constraints(&self, table: &str) -> Vec<(Option<String>, uqa_sql::ast::Expr)> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let mut out: Vec<(Option<String>, uqa_sql::ast::Expr)> = Vec::new();
        for col in t.columns.read().iter() {
            if let Some(expr) = col.check.clone() {
                out.push((Some(format!("{}_check", col.name)), expr));
            }
        }
        for c in t.table_checks.read().iter() {
            out.push((c.name.clone(), c.expr.clone()));
        }
        out
    }

    /// Snapshot of every FOREIGN KEY constraint that applies to
    /// `table`. Column-level `REFERENCES` are lifted to single-column
    /// `ForeignKey` entries.
    pub fn foreign_keys(&self, table: &str) -> Vec<uqa_sql::ast::ForeignKey> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let mut out: Vec<uqa_sql::ast::ForeignKey> = t.foreign_keys.read().clone();
        for col in t.columns.read().iter() {
            if let Some(reference) = col.references.clone() {
                out.push(uqa_sql::ast::ForeignKey {
                    name: Some(format!("{}_fkey", col.name)),
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table,
                    ref_columns: vec![reference.column],
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                });
            }
        }
        out
    }

    /// Tables that hold a FOREIGN KEY pointing at `table`. Used by
    /// DELETE / DROP CASCADE to refuse the operation when a referrer
    /// has at least one row matching the target value.
    pub fn referrers_to(&self, table: &str) -> Vec<(String, uqa_sql::ast::ForeignKey)> {
        let mut out: Vec<(String, uqa_sql::ast::ForeignKey)> = Vec::new();
        let names: Vec<String> = self.tables.read().keys().cloned().collect();
        for other in names {
            if other == table {
                continue;
            }
            for fk in self.foreign_keys(&other) {
                if fk.ref_table == table {
                    out.push((other.clone(), fk));
                }
            }
        }
        out
    }

    /// Names of columns with a `UNIQUE` or `PRIMARY KEY` constraint
    /// declared on the table. Auto-increment columns are excluded
    /// because the engine guarantees their uniqueness through the
    /// monotonic id watermark, so re-checking is redundant.
    pub fn unique_columns(&self, table: &str) -> Vec<String> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let cols = t.columns.read();
        cols.iter()
            .filter(|c| (c.unique || c.primary_key) && !c.auto_increment)
            .map(|c| c.name.clone())
            .collect()
    }

    /// Allocate the next id from the per-table watermark, returning the
    /// allocated value. Updates the watermark in place.
    pub(crate) fn allocate_next_id(&self, table: &str) -> Result<u64, SQLError> {
        let t = self
            .table(table)
            .ok_or_else(|| SQLError::Internal(format!("unknown table `{table}`")))?;
        let mut g = t.next_id.lock();
        let id = *g;
        *g = id.saturating_add(1);
        Ok(id)
    }

    /// Move the watermark past `doc_id` if needed (called after a manual
    /// id assignment so the next allocation does not collide).
    pub(crate) fn advance_next_id(&self, table: &str, doc_id: DocId) {
        let Some(t) = self.table(table) else {
            return;
        };
        let mut g = t.next_id.lock();
        if doc_id >= *g {
            *g = doc_id + 1;
        }
    }

    /// Append a column to the schema. No data migration is needed because
    /// the document store is sparse; rows missing the column read back as
    /// `Value::Null`.
    pub fn register_column(&self, table: &str, column: uqa_sql::ast::ColumnDef) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        if t.columns.read().iter().any(|c| c.name == column.name) {
            return;
        }
        t.columns.write().push(column);
        self.mark_column_stats_dirty(&table_name, &t);
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
    }

    pub fn drop_column(&self, table: &str, column: &str) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        {
            let mut cols = t.columns.write();
            cols.retain(|c| c.name != column);
        }
        // Remove from FTS field list if present.
        {
            let mut fts = t.fts_fields.write();
            fts.retain(|f| f != column);
        }
        // Drop the vector index for this field if it exists.
        {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(column) {
                idx.clear();
            }
        }
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id) else {
                continue;
            };
            if doc.remove(column).is_some() {
                self.rewrite_document(&table_name, doc_id, doc);
            }
        }
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        {
            let mut cols = t.columns.write();
            for c in cols.iter_mut() {
                if c.name == from {
                    c.name = to.to_string();
                }
            }
        }
        {
            let mut fts = t.fts_fields.write();
            for f in fts.iter_mut() {
                if f == from {
                    *f = to.to_string();
                }
            }
        }
        let vector_dimensions = {
            let mut vs = t.vector_indexes.write();
            vs.remove(from).map(|mut idx| {
                let dimensions = idx.dimensions();
                idx.clear();
                dimensions
            })
        };
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id) else {
                continue;
            };
            if let Some(value) = doc.remove(from) {
                doc.insert(to.to_string(), value);
                self.rewrite_document(&table_name, doc_id, doc);
            }
        }
        if let Some(dimensions) = vector_dimensions {
            self.create_vector_field(&table_name, to, dimensions);
        }
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn rename_table(&self, from: &str, to: &str) -> bool {
        let Some(from) = self.resolve_table_name(from) else {
            return false;
        };
        let to = self.relation_name_for_create(to);
        let mut tables = self.tables.write();
        if tables.contains_key(&to) {
            return false;
        }
        let Some(state) = tables.remove(&from) else {
            return false;
        };
        tables.insert(to.clone(), state.clone());
        drop(tables);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(&from);
        }
        if self.is_persistent() {
            self.save_table_schema(&to, &state);
        }
        self.mark_column_stats_dirty(&to, &state);
        true
    }

    /// Append `field` to the table's FTS field list. Existing rows are
    /// indexed immediately so SQL `CREATE INDEX USING gin` behaves like a
    /// real secondary-index build rather than a metadata-only toggle.
    pub fn add_fts_field(&self, table: &str, field: FieldName) -> Result<(), String> {
        self.add_fts_field_with_analyzer(table, field, None)
    }

    /// Same as [`Engine::add_fts_field`], but allows registering a
    /// per-field analyzer name (e.g. `standard_cjk`). When `None`, the
    /// table-level analyzer continues to apply.
    pub fn add_fts_field_with_analyzer(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let table_name = self
            .resolve_table_name(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .table(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("add_fts_field: {e}"))?;
            self.table_field_analyzers.write().insert(
                (table_name.clone(), field.clone()),
                (analyzer_name.to_string(), "both".to_string()),
            );
            if let Some(catalog) = self.catalog.as_ref() {
                let _ =
                    catalog.save_table_field_analyzer(&table_name, &field, "both", analyzer_name);
            }
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field);
            }
        }
        Self::rebuild_fts_index(&t)?;
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        Ok(())
    }

    pub fn get_document(&self, table: &str, doc_id: DocId) -> Option<Document> {
        let t = self.table(table)?;
        let got = t.document_store.read().get(doc_id);
        got
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> BTreeMap<DocId, Value> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        let values = t.document_store.read().get_fields_bulk(doc_ids, field);
        values
    }

    pub fn find_doc_id_by_field(&self, table: &str, field: &str, value: &Value) -> Option<DocId> {
        let t = self.table(table)?;
        let found = t.document_store.read().find_doc_id_by_field(field, value);
        found
    }

    /// Find the first document whose conflict columns all match the
    /// given values. Returns the existing doc id when a conflict
    /// exists, `None` when the row would be a fresh insert. Mirrors
    /// `PostgreSQL`'s `ON CONFLICT (col, ...)` lookup; the conflict
    /// columns map to the unique-constraint target. The UQA-RS implementation
    /// scans every document because the planner does not yet support
    /// secondary unique indexes; the lookup is still correct for
    /// small / medium tables, which is where UPSERT is most useful.
    pub fn find_conflict(
        &self,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Option<DocId> {
        if conflict_columns.is_empty() || conflict_columns.len() != values.len() {
            return None;
        }
        let t = self.table(table)?;
        let snap = t.document_store.read().snapshot();
        for doc_id in snap.doc_ids() {
            let Some(doc) = snap.get(doc_id) else {
                continue;
            };
            let mut all_match = true;
            for (col, want) in conflict_columns.iter().zip(values.iter()) {
                let got = doc.get(col).cloned().unwrap_or(Value::Null);
                if &got != want {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                return Some(doc_id);
            }
        }
        None
    }

    /// Apply per-column updates to an existing document. Mirrors the
    /// `DO UPDATE SET col = expr` branch of an ON CONFLICT clause.
    /// Returns whether the row was updated; `false` when the document
    /// no longer exists.
    pub fn update_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<f32>>,
    ) -> bool {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.update_document_fields_with_vector_values(table, doc_id, updates, vector_values)
    }

    pub fn update_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let Some(mut doc) = t.document_store.read().get(doc_id) else {
            return false;
        };
        for (k, v) in updates {
            doc.insert(k, v);
        }
        // Re-add the document so the inverted index picks up the new
        // text fields.
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(table, doc_id, doc, vectors);
        true
    }

    /// Apply field-level updates without materialising the whole
    /// document. Callers must only use this path when constraints and
    /// referential actions do not need the old or complete new row.
    pub fn patch_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> bool {
        let vector_values: BTreeMap<String, Vec<Vec<f32>>> = vectors
            .iter()
            .map(|(field, vector)| (field.clone(), vec![vector.clone()]))
            .collect();
        self.patch_document_fields_with_vector_values(table, doc_id, updates, &vector_values)
    }

    pub fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };

        let fts_fields = t.fts_fields();
        let touches_fts = updates
            .keys()
            .any(|field| fts_fields.iter().any(|fts| fts == field));
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        if touches_fts {
            let store = t.document_store.read();
            for field in &fts_fields {
                let value = updates
                    .get(field)
                    .cloned()
                    .or_else(|| store.get_field(doc_id, field));
                if let Some(Value::Str(text)) = value {
                    text_fields.insert(field.clone(), text);
                }
            }
        }

        if !t.document_store.write().patch_fields(doc_id, updates) {
            return false;
        }

        if touches_fts {
            let mut index = t.inverted_index.write();
            index.remove_document(doc_id);
            if !text_fields.is_empty() {
                index.add_document(doc_id, text_fields);
            }
        }

        {
            let mut indexes = t.vector_indexes.write();
            for (field, index) in indexes.iter_mut() {
                if !updates.contains_key(field) {
                    continue;
                }
                index.delete(doc_id);
                if let Some(values) = vectors.get(field) {
                    index.add_many(doc_id, values.clone());
                }
            }
        }

        self.mark_column_stats_dirty(table, &t);
        true
    }

    pub(crate) fn rewrite_document(&self, table: &str, doc_id: DocId, document: Document) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        let vector_fields: Vec<FieldName> = t.vector_indexes.read().keys().cloned().collect();
        let mut vectors: BTreeMap<FieldName, Vec<Vec<f32>>> = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(&t, &field, value) {
                vectors.insert(field, values);
            }
        }
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(&table_name, doc_id, document, vectors);
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn document_count(&self, table: &str) -> u64 {
        self.table(table)
            .map_or(0, |t| t.inverted_index.read().doc_count())
    }

    /// Snapshot a table into an [`ExecutionContext`] together with an
    /// `Arc<IndexStats>` that scorers can hold without juggling lifetimes.
    fn snapshot_context(
        &self,
        table: &str,
    ) -> Option<(ExecutionContext, Arc<uqa_core::IndexStats>)> {
        let t = self.table(table)?;
        let inv = t.inverted_index.read().snapshot();
        let stats = inv.stats();
        let stats_arc = Arc::new(stats.clone());
        let docs = t.document_store.read().snapshot();

        let mut ctx = ExecutionContext::new()
            .with_inverted_index(inv)
            .with_document_store(docs)
            .with_stats(stats);

        for (field, idx) in t.vector_indexes.read().iter() {
            ctx = ctx.with_vector_index(field.clone(), idx.snapshot());
        }

        Some((ctx, stats_arc))
    }

    fn build_text_scorer(
        mode: &ScoringMode,
        stats_arc: Arc<uqa_core::IndexStats>,
    ) -> Arc<dyn Scorer> {
        match mode {
            ScoringMode::BM25(p) => Arc::new(BM25Scorer::new(*p, stats_arc)),
            ScoringMode::BayesianBM25(p) => Arc::new(BayesianBM25Scorer::new(*p, stats_arc)),
        }
    }

    fn rank_top_k(pl: &PostingList, top_k: usize) -> Vec<ScoredEntry> {
        let mut entries: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
        entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        entries.truncate(top_k);
        entries
    }

    /// Run a single-term or multi-term `text_match` query against `field`
    /// with the chosen scoring mode and return the top-`k` entries.
    pub fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Vec<ScoredEntry> {
        let Some((ctx, stats_arc)) = self.snapshot_context(table) else {
            return Vec::new();
        };
        let analyzer = ctx
            .inverted_index
            .as_ref()
            .expect("snapshot_context populates the inverted index")
            .get_search_analyzer(field);
        let analyzed_terms = analyzer.analyze(query);
        if analyzed_terms.is_empty() {
            return Vec::new();
        }
        let term_op: Arc<dyn Operator> = Arc::new(TermOperator::new(query, field));
        let scorer = Self::build_text_scorer(mode, stats_arc);
        let score_op = ScoreOperator::new(scorer, term_op, analyzed_terms, field);
        let result = score_op.execute(&ctx);
        Self::rank_top_k(&result, top_k)
    }

    /// Compute calibration diagnostics for a Bayesian BM25 query
    /// against every document in `table`, aligned to `labels` in
    /// ascending document-id order.
    pub fn calibration_report(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<CalibrationReport, SQLError> {
        if self.table(table).is_none() {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
        let doc_ids = self.table_doc_ids(table);
        if labels.len() != doc_ids.len() {
            return Err(SQLError::TypeMismatch(format!(
                "labels length ({}) must match document count ({})",
                labels.len(),
                doc_ids.len()
            )));
        }

        let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let probabilities: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| score_map.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        Ok(CalibrationMetrics::report(&probabilities, labels, 10))
    }

    pub fn learn_scoring_params(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<std::collections::BTreeMap<String, f64>, SQLError> {
        if self.table(table).is_none() {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
        let doc_ids = self.table_doc_ids(table);
        if labels.len() != doc_ids.len() {
            return Err(SQLError::TypeMismatch(format!(
                "labels length ({}) must match document count ({})",
                labels.len(),
                doc_ids.len()
            )));
        }

        let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let scores: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| score_map.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let labels_f: Vec<f64> = labels.iter().map(|label| f64::from(*label)).collect();
        let mut learner = ParameterLearner::default();
        let params = learner.fit_with_options(&scores, &labels_f, None, None);
        let json = serde_json::to_string(&params)
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)?;
        Ok(params)
    }

    pub fn update_scoring_params(
        &self,
        table: &str,
        field: &str,
        score: f64,
        label: u8,
    ) -> Result<(), SQLError> {
        let key = format!("{table}.{field}");
        let mut learner = if let Some(json) = self.load_scoring_params(&key) {
            let params: std::collections::BTreeMap<String, f64> =
                serde_json::from_str(&json).unwrap_or_default();
            ParameterLearner::new(
                params.get("alpha").copied().unwrap_or(1.0),
                params.get("beta").copied().unwrap_or(0.0),
                Some(params.get("base_rate").copied().unwrap_or(0.5)),
            )
        } else {
            ParameterLearner::default()
        };
        learner.update(score, f64::from(label), 1.0, 1.0, 0.1);
        let json = serde_json::to_string(&learner.params())
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&key, &json)
    }

    /// Top-`k` nearest neighbors against the named vector field.
    pub fn knn_search(
        &self,
        table: &str,
        field: &str,
        query_vector: Vec<f32>,
        top_k: usize,
    ) -> Vec<ScoredEntry> {
        let Some((ctx, _)) = self.snapshot_context(table) else {
            return Vec::new();
        };
        let knn = KNNOperator::new(query_vector, top_k, field);
        let pl = knn.execute(&ctx);
        Self::rank_top_k(&pl, top_k)
    }

    /// All documents whose cosine similarity to `query_vector` is at least
    /// `threshold`.
    pub fn vector_similarity_search(
        &self,
        table: &str,
        field: &str,
        query_vector: Vec<f32>,
        threshold: f32,
    ) -> Vec<ScoredEntry> {
        let Some((ctx, _)) = self.snapshot_context(table) else {
            return Vec::new();
        };
        let op = VectorSimilarityOperator::new(query_vector, threshold, field);
        let pl = op.execute(&ctx);
        let mut out: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        out
    }

    /// Hybrid search: Bayesian BM25 over `text_field` AND KNN over
    /// `vector_field`, combined via log-odds conjunction (Section 4,
    /// Paper 4). Both signals are pre-calibrated to (0, 1) before
    /// fusion: BM25 via the three-term posterior, vector via
    /// `cosine_to_probability`. Returns top-`top_k` by fused score
    /// descending.
    pub fn hybrid_search(&self, params: &HybridSearchParams) -> Vec<ScoredEntry> {
        let Some((ctx, stats_arc)) = self.snapshot_context(params.table) else {
            return Vec::new();
        };
        let analyzer = ctx
            .inverted_index
            .as_ref()
            .expect("snapshot_context populates the inverted index")
            .get_search_analyzer(params.text_field);
        let analyzed_terms = analyzer.analyze(params.text_query);
        if analyzed_terms.is_empty() && !ctx.vector_indexes.contains_key(params.vector_field) {
            return Vec::new();
        }

        let mut signals: Vec<Arc<dyn Operator>> = Vec::new();

        if !analyzed_terms.is_empty() {
            let term_op: Arc<dyn Operator> =
                Arc::new(TermOperator::new(params.text_query, params.text_field));
            let bayes = Arc::new(BayesianBM25Scorer::new(
                BayesianBM25Params::default(),
                stats_arc,
            )) as Arc<dyn Scorer>;
            let scored: Arc<dyn Operator> = Arc::new(ScoreOperator::new(
                bayes,
                term_op,
                analyzed_terms,
                params.text_field,
            ));
            signals.push(scored);
        }

        if ctx.vector_indexes.contains_key(params.vector_field) {
            let knn: Arc<dyn Operator> = Arc::new(KNNOperator::new(
                params.query_vector.clone(),
                params.knn_pool,
                params.vector_field,
            ));
            let cosine_prob: Arc<dyn Operator> = Arc::new(CosineProbabilityOperator::new(knn));
            signals.push(cosine_prob);
        }

        if signals.is_empty() {
            return Vec::new();
        }

        let fusion = LogOddsFusionOperator::new(signals, params.alpha);
        let result = fusion.execute(&ctx);
        Self::rank_top_k(&result, params.top_k)
    }
}

fn default_runtime_parameter(name: &str) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("server_version") {
        return Some("17.0-uqa");
    }
    if name.eq_ignore_ascii_case("server_encoding")
        || name.eq_ignore_ascii_case("client_encoding")
    {
        return Some("UTF8");
    }
    if name.eq_ignore_ascii_case("datestyle") {
        return Some("ISO, MDY");
    }
    if name.eq_ignore_ascii_case("timezone") {
        return Some("UTC");
    }
    None
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
    fn run_subquery(
        &self,
        stmt: &uqa_sql::ast::SelectStmt,
        outer_row: Option<&uqa_sql::result::ResultRow>,
        params: &[uqa_sql::SQLParam],
    ) -> std::result::Result<(Vec<String>, Vec<uqa_sql::result::ResultRow>), String> {
        let ctes = BTreeMap::new();
        match crate::sql::run_correlated_subquery(self, stmt, outer_row, params, &ctes) {
            Ok(r) => Ok((r.columns, r.rows)),
            Err(e) => Err(format!("subquery failed: {e}")),
        }
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
    /// Confidence-scaling exponent for log-odds fusion (Paper 4 Section 4).
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
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        other => Err(format!("expected feature array, got {other:?}")),
    }
}

fn value_to_usize(value: &Value) -> Result<usize, String> {
    match value {
        Value::Int(value) if *value >= 0 => Ok(*value as usize),
        Value::Float(value) if value.fract() == 0.0 && *value >= 0.0 => Ok(*value as usize),
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

fn distinct_count(values: &[Value]) -> u64 {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<&Value> = BTreeSet::new();
    for v in values {
        set.insert(v);
    }
    set.len() as u64
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
        let table = engine.table(table).expect("table");
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
    }

    impl StoreWithMissingDocId {
        fn from_table(engine: &Engine, table: &str, missing_doc_id: DocId) -> Self {
            let table = engine.table(table).expect("table");
            let docs = table.document_store.read().iter_all().collect();
            Self {
                docs,
                missing_doc_id,
            }
        }
    }

    impl DocumentStore for StoreWithMissingDocId {
        fn put(&mut self, doc_id: DocId, document: Document) {
            self.docs.insert(doc_id, document);
        }

        fn get(&self, doc_id: DocId) -> Option<Document> {
            self.docs.get(&doc_id).cloned()
        }

        fn delete(&mut self, doc_id: DocId) {
            self.docs.remove(&doc_id);
        }

        fn clear(&mut self) {
            self.docs.clear();
        }

        fn doc_ids(&self) -> Vec<DocId> {
            let mut ids = vec![self.missing_doc_id];
            ids.extend(self.docs.keys().copied());
            ids
        }

        fn len(&self) -> usize {
            self.docs.len() + 1
        }

        fn snapshot(&self) -> Arc<dyn DocumentStore> {
            Arc::new(self.clone())
        }
    }

    #[test]
    fn sql_update_skips_stale_document_ids() {
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
            let table = eng.table("docs").expect("table");
            *table.document_store.write() =
                Box::new(StoreWithMissingDocId::from_table(&eng, "docs", 99));
        }

        let result = eng
            .sql(
                "UPDATE docs
                    SET content = 'updated content',
                        status = 'indexed'
                  WHERE id = 1 AND status = 'queued'",
                &[],
            )
            .unwrap();

        assert_eq!(result.affected_rows, 1);
        let doc = eng.get_document("docs", 1).unwrap();
        assert_eq!(doc.get("content"), Some(&s("updated content")));
        assert_eq!(doc.get("status"), Some(&s("indexed")));
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
            eng.create_default_table("docs", vec!["title".into()]);
            eng.add_document("docs", 1, doc([("title", s("hello facade"))]));
        }

        let reopened = Engine::from_persistent_backends(catalog, backend).unwrap();
        assert_eq!(reopened.document_count("docs"), 1);
        let hits = reopened.search("docs", "title", "facade", &ScoringMode::default(), 10);
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
                  WHERE table_name = 'articles' AND field = 'embedding'
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
                  WHERE table_name = 'articles' AND field = 'embedding'",
                [],
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO _ivf_assignments
                        (table_name, field, doc_id, centroid_id)
                     VALUES ('articles', 'embedding', 1, {other})"
                ),
                [],
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO _ivf_assignments
                        (table_name, field, doc_id, centroid_id)
                     VALUES ('articles', 'embedding', 2, {nearest})"
                ),
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn run_analyze_populates_column_stats() {
        let eng = Engine::new();
        eng.create_default_table("docs", vec!["title".into()]);
        // Register the columns directly through the table state so we
        // don't depend on the SQL DDL path here.
        if let Some(t) = eng.table("docs") {
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
        eng.add_document("docs", 1, doc([("title", s("alpha"))]));
        eng.add_document("docs", 2, doc([("title", s("alpha"))]));
        eng.add_document("docs", 3, doc([("title", s("beta"))]));
        eng.run_analyze(Some("docs"));
        let stats = eng.column_stats("docs");
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
        eng.create_default_table("articles", vec!["title".into()]);
        eng.add_document("articles", 1, doc([("title", s("rust language"))]));
        let got = eng.get_document("articles", 1).unwrap();
        assert_eq!(got.get("title"), Some(&s("rust language")));
        eng.delete_document("articles", 1);
        assert!(eng.get_document("articles", 1).is_none());
    }

    #[test]
    fn search_returns_top_k_bm25_in_score_order() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()]);
        eng.add_document(
            "articles",
            1,
            doc([("title", s("the rust programming language"))]),
        );
        eng.add_document("articles", 2, doc([("title", s("python language guide"))]));
        eng.add_document("articles", 3, doc([("title", s("rust rust rust"))]));

        let hits = eng.search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BM25(BM25Params::default()),
            10,
        );
        // Doc 3 has tf=3 and is shorter -> highest BM25.
        assert_eq!(hits.first().map(|h| h.doc_id), Some(3));
        assert!(hits.iter().any(|h| h.doc_id == 1));
        assert!(hits.iter().all(|h| h.doc_id != 2));
    }

    #[test]
    fn search_returns_calibrated_probabilities_under_bayesian_bm25() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()]);
        eng.add_document(
            "articles",
            1,
            doc([("title", s("the rust programming language"))]),
        );
        eng.add_document("articles", 2, doc([("title", s("python is dynamic"))]));

        let hits = eng.search(
            "articles",
            "title",
            "rust",
            &ScoringMode::BayesianBM25(BayesianBM25Params::default()),
            10,
        );

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
        eng.create_default_table("articles", vec!["title".into()]);
        eng.create_vector_field("articles", "embedding", 3);
        eng.add_document_with_vectors(
            "articles",
            1,
            doc([("title", s("a"))]),
            BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
        );
        eng.add_document_with_vectors(
            "articles",
            2,
            doc([("title", s("b"))]),
            BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
        );
        eng.add_document_with_vectors(
            "articles",
            3,
            doc([("title", s("c"))]),
            BTreeMap::from([("embedding".into(), vec![0.7, 0.7, 0.0])]),
        );

        let hits = eng.knn_search("articles", "embedding", vec![1.0, 0.0, 0.0], 2);
        assert_eq!(hits.first().map(|h| h.doc_id), Some(1));
        // doc 3 (cos ~0.707) beats doc 2 (cos 0.0).
        assert_eq!(hits.get(1).map(|h| h.doc_id), Some(3));
    }

    #[test]
    fn vector_fields_use_ivf_backends() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec![]);
        eng.create_vector_field("articles", "embedding", 3);
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
        let hits = reopened.knn_search("articles", "embedding", vec![1.0, 0.0], 1);
        assert_eq!(hits.first().map(|h| h.doc_id), Some(2));
    }

    #[test]
    fn create_vector_field_backfills_existing_documents() {
        let eng = Engine::new();
        eng.create_default_table("docs", vec![]);
        eng.add_document("docs", 1, doc([("embedding", vector(&[1.0, 0.0]))]));
        eng.add_document("docs", 2, doc([("embedding", vector(&[0.0, 1.0]))]));
        eng.add_document("docs", 3, doc([("embedding", vector(&[0.8, 0.2]))]));

        assert!(eng.create_vector_field("docs", "embedding", 2));
        let hits = eng.knn_search("docs", "embedding", vec![1.0, 0.0], 2);
        assert_eq!(
            hits.iter().map(|h| h.doc_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn hybrid_search_combines_text_and_vector_signals() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()]);
        eng.create_vector_field("articles", "embedding", 3);

        // Doc 1: title matches "rust", embedding pointing toward query.
        eng.add_document_with_vectors(
            "articles",
            1,
            doc([("title", s("rust language"))]),
            BTreeMap::from([("embedding".into(), vec![1.0, 0.0, 0.0])]),
        );
        // Doc 2: title matches "rust", embedding orthogonal to query.
        eng.add_document_with_vectors(
            "articles",
            2,
            doc([("title", s("rust ecosystem"))]),
            BTreeMap::from([("embedding".into(), vec![0.0, 1.0, 0.0])]),
        );
        // Doc 3: no text match, embedding near query.
        eng.add_document_with_vectors(
            "articles",
            3,
            doc([("title", s("python programming"))]),
            BTreeMap::from([("embedding".into(), vec![0.95, 0.1, 0.0])]),
        );

        let hits = eng.hybrid_search(&HybridSearchParams {
            table: "articles",
            text_field: "title",
            text_query: "rust",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0, 0.0],
            knn_pool: 10,
            alpha: 0.5,
            top_k: 10,
        });

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
        eng.create_default_table("articles", vec!["title".into()]);
        for i in 0..5 {
            eng.add_document("articles", i, doc([("title", s(&format!("doc {i}")))]));
        }
        assert_eq!(eng.document_count("articles"), 5);
    }
}
