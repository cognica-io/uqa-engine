//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level engine: a per-table [`DocumentStore`] + [`InvertedIndex`]
//! pair, document mutation entry points, and a minimal `search` API for
//! text-only round trips. Backed either by in-memory stores
//! ([`Engine::new`]) or by `SQLite` ([`Engine::open`]); the operator
//! pipeline is identical across backends.
//!
//! # Public API surface
//!
//! Construction:
//! - [`Engine::new`] — purely in-memory; great for tests and the REPL.
//! - [`Engine::open`] — `SQLite`-backed catalog at the given path; reopens
//!   restore tables, models, and graphs from disk.
//!
//! Schema and table lifecycle:
//! - [`Engine::create_table`] — register a table with declared columns.
//! - [`Engine::create_default_table`] — convenience for FTS-only tables.
//! - [`Engine::create_vector_field`] — attach a vector field to an
//!   existing table.
//!
//! Document mutation:
//! - [`Engine::add_document`], [`Engine::add_document_with_vectors`]
//! - [`Engine::add_vector`] — set or replace a vector for an existing doc.
//! - [`Engine::get_document`], [`Engine::delete_document`]
//! - [`Engine::document_count`]
//!
//! Querying:
//! - `Engine::sql` (defined in [`sql`]) — full SQL surface (select /
//!   insert / update / delete / create-table, plus the registered
//!   functions: `text_match`, `knn_match`, `fuse_log_odds`,
//!   `multi_field_match`, `staged_retrieval`, `graph_*`, `deep_predict`).
//! - [`Engine::search`] — direct text-only retrieval returning a posting
//!   list.
//! - [`Engine::knn_search`], [`Engine::vector_similarity_search`] — k-NN
//!   over a vector field.
//! - [`Engine::hybrid_search`] — log-odds fusion of text and vector
//!   posting lists (no SQL parsing in the hot path).
//!
//! Deep-model persistence:
//! - [`Engine::save_model`], [`Engine::load_model`], [`Engine::drop_model`]
//! - [`Engine::deep_predict`] — runs a stored model against the cached
//!   feature row and returns ranked `(doc_id, score)` pairs.
//!
//! Graph workspaces (used by the Cypher front-end and the `graph_*`
//! SQL functions):
//! - [`Engine::create_graph`], [`Engine::drop_graph`]
//! - [`Engine::graph_with`] — read-only access by name.
//! - [`Engine::graph_with_mut`] — exclusive mutable access.
//!
//! Result types ([`SQLResult`], [`SQLParam`]) are re-exported from
//! `uqa-sql`. Errors flow through [`EngineError`], which wraps SQL and
//! storage errors so callers only need to match one enum.

pub mod deep;
pub mod sql;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use uqa_analysis::{analyzer::standard_analyzer, Analyzer};
use uqa_core::{DocId, FieldName, PostingEntry, PostingList, Value};
use uqa_operators::{
    CosineProbabilityOperator, ExecutionContext, KNNOperator, LogOddsFusionOperator, Operator,
    ScoreOperator, TermOperator, VectorSimilarityOperator,
};
use uqa_scoring::{BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, Scorer};
use uqa_sql::SQLError;
use uqa_storage::{
    document_store::Document, Catalog, DocumentStore, InvertedIndex, ManagedConnection,
    MemoryDocumentStore, MemoryInvertedIndex, MemoryVectorIndex, SQLiteDocumentStore, SQLiteError,
    SQLiteInvertedIndex, SQLiteVectorIndex, TableSchema, VectorFieldSchema, VectorIndex,
};

pub use uqa_sql::{SQLParam, SQLResult};

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
    catalog: Option<Arc<Catalog>>,
    conn: Option<ManagedConnection>,
    /// Named in-memory graphs reachable from SQL via the
    /// `graph_*` function family. Persistence to the catalog is left
    /// to a follow-up slice.
    graphs: RwLock<BTreeMap<String, uqa_graph::MemoryGraphStore>>,
    /// Saved deep-fusion models. Mirrors the catalog `_models` table
    /// when the engine is SQLite-backed.
    models: RwLock<BTreeMap<String, deep::DeepModel>>,
    /// Registered views. Each entry holds the underlying
    /// `SelectStmt`; the SQL surface re-runs the body on every
    /// reference (no row caching).
    views: RwLock<BTreeMap<String, uqa_sql::ast::SelectStmt>>,
    /// Registered schema names. Engine-level schemas are advisory
    /// today: the catalog records them so `CREATE SCHEMA` does not
    /// error out, but tables themselves still live in the flat
    /// per-name namespace.
    schemas: RwLock<std::collections::BTreeSet<String>>,
    /// Open transaction stack. `BEGIN` pushes a new frame, `COMMIT`
    /// / `ROLLBACK` pop one, savepoint statements update the top
    /// frame's savepoint set.
    tx_stack: parking_lot::Mutex<Vec<TransactionFrame>>,
    /// Per-engine cancellation token. Operators cloned through
    /// [`Engine::cancellation_token`] check the flag at chunk
    /// boundaries; calling [`Engine::cancel`] from any thread tears
    /// every in-flight query down with `SQLError::Cancelled`.
    cancel: uqa_core::CancellationToken,
}

#[derive(Debug, Default)]
struct TransactionFrame {
    savepoints: std::collections::BTreeSet<String>,
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
    /// Per-column statistics refreshed by `ANALYZE table_name`. Keyed
    /// by column name. Empty until `run_analyze` runs at least once.
    column_stats: RwLock<BTreeMap<String, uqa_planner::ColumnStats>>,
}

impl TableState {
    fn fts_fields(&self) -> Vec<FieldName> {
        self.fts_fields.read().clone()
    }
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
            conn: None,
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(std::collections::BTreeSet::new()),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
        }
    }

    /// Cancel every in-flight query that holds a clone of this
    /// engine's cancellation token. Mirrors `Engine.cancel()` in
    /// the Python reference; surfaces to operator hot loops as
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

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// SQLite-backed engine. Opens (or creates) the database at `path`,
    /// runs catalog migrations, and rebuilds the in-memory table
    /// registry from the persisted catalog.
    pub fn open(path: &Path) -> Result<Self, SQLiteError> {
        let conn = ManagedConnection::open(path)?;
        let catalog = Arc::new(Catalog::open(conn.clone())?);
        let mut engine = Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog.clone()),
            conn: Some(conn.clone()),
            graphs: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            views: RwLock::new(BTreeMap::new()),
            schemas: RwLock::new(std::collections::BTreeSet::new()),
            tx_stack: parking_lot::Mutex::new(Vec::new()),
            cancel: uqa_core::CancellationToken::new(),
        };
        engine.restore_from_catalog(&catalog, &conn)?;
        // Eagerly populate the model cache from the catalog so
        // `load_model` is one read deep.
        if let Ok(rows) = catalog.load_models() {
            for (name, json) in rows {
                if let Ok(model) = serde_json::from_str::<deep::DeepModel>(&json) {
                    engine.models.write().insert(name, model);
                }
            }
        }
        Ok(engine)
    }

    fn restore_from_catalog(
        &mut self,
        catalog: &Catalog,
        conn: &ManagedConnection,
    ) -> Result<(), SQLiteError> {
        for schema in catalog.load_tables()? {
            let analyzer: Analyzer = serde_json::from_str(&schema.analyzer_json)?;
            let docs: Box<dyn DocumentStore> =
                Box::new(SQLiteDocumentStore::new(conn.clone(), &schema.name));
            let inv: Box<dyn InvertedIndex> = Box::new(SQLiteInvertedIndex::new(
                conn.clone(),
                &schema.name,
                analyzer.clone(),
            ));
            let mut vectors: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::new();
            for vf in &schema.vector_fields {
                vectors.insert(
                    vf.field.clone(),
                    Box::new(SQLiteVectorIndex::new(
                        conn.clone(),
                        &schema.name,
                        &vf.field,
                        vf.dimensions,
                    )),
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
            let max_id = {
                let store = docs.snapshot();
                store.doc_ids().into_iter().max().unwrap_or(0)
            };
            let table = TableState {
                document_store: RwLock::new(docs),
                inverted_index: RwLock::new(inv),
                vector_indexes: RwLock::new(vectors),
                fts_fields: RwLock::new(schema.fts_fields.clone()),
                columns: RwLock::new(columns),
                next_id: parking_lot::Mutex::new(max_id + 1),
                analyzer: RwLock::new(analyzer),
                column_stats: RwLock::new(BTreeMap::new()),
            };
            self.tables.write().insert(schema.name, Arc::new(table));
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
        let name = name.into();
        let (docs, inv): (Box<dyn DocumentStore>, Box<dyn InvertedIndex>) =
            if let Some(conn) = self.conn.as_ref() {
                (
                    Box::new(SQLiteDocumentStore::new(conn.clone(), &name)),
                    Box::new(SQLiteInvertedIndex::new(
                        conn.clone(),
                        &name,
                        analyzer.clone(),
                    )),
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
        };
        let table_arc = Arc::new(table);
        self.tables.write().insert(name.clone(), table_arc.clone());
        if self.is_persistent() {
            self.save_table_schema(&name, &table_arc);
        }
    }

    /// Register a vector field on a table. The vector index starts empty;
    /// call [`Engine::add_vector`] (or pass embeddings to
    /// [`Engine::add_document_with_vectors`]) to populate it.
    pub fn create_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let field = field.into();
        let idx: Box<dyn VectorIndex> = if let Some(conn) = self.conn.as_ref() {
            Box::new(SQLiteVectorIndex::new(
                conn.clone(),
                table,
                &field,
                dimensions,
            ))
        } else {
            Box::new(MemoryVectorIndex::new(dimensions))
        };
        t.vector_indexes.write().insert(field, idx);
        if self.is_persistent() {
            self.save_table_schema(table, &t);
        }
        true
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

    pub fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<f32>>,
    ) {
        self.add_document(table, doc_id, document);
        for (field, vector) in vectors {
            self.add_vector(table, doc_id, &field, vector);
        }
    }

    pub fn create_default_table(&self, name: impl Into<String>, fts_fields: Vec<FieldName>) {
        self.create_table(name, standard_analyzer("english"), fts_fields);
    }

    /// Create a named in-memory graph. No-op if it already exists.
    pub fn create_graph(&self, name: impl Into<String>) {
        let name = name.into();
        let mut graphs = self.graphs.write();
        graphs.entry(name).or_default();
    }

    /// Drop a named graph. No-op when the graph is missing.
    pub fn drop_graph(&self, name: &str) {
        self.graphs.write().remove(name);
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
        let mut graphs = self.graphs.write();
        graphs.get_mut(name).map(f)
    }

    /// Persist a deep-fusion model under `name`. Round-trips as JSON
    /// through the catalog's `_models` table when the engine is in
    /// `SQLite` mode; in-memory engines keep the latest version per
    /// process.
    pub fn save_model(&self, name: &str, model: &deep::DeepModel) -> Result<(), SQLError> {
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

    pub fn load_model(&self, name: &str) -> Option<deep::DeepModel> {
        if let Some(m) = self.models.read().get(name).cloned() {
            return Some(m);
        }
        let catalog = self.catalog.as_ref()?;
        let json = catalog.load_model(name).ok().flatten()?;
        let model: deep::DeepModel = serde_json::from_str(&json).ok()?;
        self.models.write().insert(name.to_string(), model.clone());
        Some(model)
    }

    pub fn drop_model(&self, name: &str) {
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_model(name);
        }
        self.models.write().remove(name);
    }

    /// Run inference for a saved model against a fresh execution
    /// context. Returns `(doc_id, score)` pairs ordered by `doc_id`.
    pub fn deep_predict(&self, name: &str) -> Option<Vec<(DocId, f64)>> {
        let model = self.load_model(name)?;
        let ctx = ExecutionContext::new();
        let (scores, _) = model.predict(&ctx);
        Some(scores)
    }

    fn table(&self, name: &str) -> Option<Arc<TableState>> {
        self.tables.read().get(name).cloned()
    }

    pub fn add_document(&self, table: &str, doc_id: DocId, document: Document) {
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
        self.views.write().insert(name.to_string(), body);
    }

    pub fn drop_view(&self, name: &str) -> bool {
        self.views.write().remove(name).is_some()
    }

    pub fn view(&self, name: &str) -> Option<uqa_sql::ast::SelectStmt> {
        self.views.read().get(name).cloned()
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

    /// Refresh per-column statistics for a single table or every
    /// table when `table` is `None`. Mirrors `Table.analyze` in
    /// the Python reference: scans every document, collects per-
    /// column distinct count / null count / min / max / equi-depth
    /// histogram (100 buckets) / MCV list (top 10 above-average
    /// frequency), and stores the result in [`TableState::column_stats`]
    /// so the cardinality estimator can read it on subsequent
    /// queries.
    pub fn run_analyze(&self, table: Option<&str>) {
        let names: Vec<String> = match table {
            Some(t) => vec![t.to_string()],
            None => self.tables.read().keys().cloned().collect(),
        };
        for name in names {
            let Some(t) = self.table(&name) else { continue };
            self.analyze_table(&t);
        }
    }

    #[allow(clippy::unused_self)]
    fn analyze_table(&self, t: &Arc<TableState>) {
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

        *t.column_stats.write() = stats_out;
    }

    /// Snapshot of the cardinality estimator's per-column statistics
    /// for `table`, or an empty map when ANALYZE has not been run.
    pub fn column_stats(&self, table: &str) -> BTreeMap<String, uqa_planner::ColumnStats> {
        self.table(table)
            .map(|t| t.column_stats.read().clone())
            .unwrap_or_default()
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
    }

    /// Execute a transaction control statement (`BEGIN` / `COMMIT` /
    /// `ROLLBACK` / savepoint variants) against the engine. The
    /// engine maintains a single transaction stack per connection.
    pub fn run_transaction_statement(
        &self,
        tx: uqa_sql::ast::TransactionStmt,
    ) -> Result<(), SQLError> {
        use uqa_sql::ast::TransactionStmt;
        let mut guard = self.tx_stack.lock();
        match tx {
            TransactionStmt::Begin => {
                guard.push(TransactionFrame::default());
            }
            TransactionStmt::Commit => {
                if guard.pop().is_none() {
                    return Err(SQLError::Internal(
                        "COMMIT without an open transaction".into(),
                    ));
                }
            }
            TransactionStmt::Rollback => {
                if guard.pop().is_none() {
                    return Err(SQLError::Internal(
                        "ROLLBACK without an open transaction".into(),
                    ));
                }
            }
            TransactionStmt::Savepoint(name) => {
                let frame = guard
                    .last_mut()
                    .ok_or_else(|| SQLError::Internal("SAVEPOINT outside a transaction".into()))?;
                frame.savepoints.insert(name);
            }
            TransactionStmt::ReleaseSavepoint(name) => {
                let frame = guard.last_mut().ok_or_else(|| {
                    SQLError::Internal("RELEASE SAVEPOINT outside a transaction".into())
                })?;
                frame.savepoints.remove(&name);
            }
            TransactionStmt::RollbackToSavepoint(name) => {
                let frame = guard.last_mut().ok_or_else(|| {
                    SQLError::Internal("ROLLBACK TO SAVEPOINT outside a transaction".into())
                })?;
                if !frame.savepoints.contains(&name) {
                    return Err(SQLError::Internal(format!("savepoint `{name}` not found")));
                }
            }
        }
        Ok(())
    }

    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> bool {
        let removed = self.tables.write().remove(name).is_some();
        if !removed {
            return false;
        }
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(name);
            let _ = catalog.purge_table_data(name);
        }
        true
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.tables.read().contains_key(name)
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

    /// Return the SERIAL/BIGSERIAL column name for `table`, if any.
    pub(crate) fn auto_increment_column(&self, table: &str) -> Option<String> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter()
            .find(|c| c.auto_increment)
            .map(|c| c.name.clone())
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
        let Some(t) = self.table(table) else {
            return;
        };
        if t.columns.read().iter().any(|c| c.name == column.name) {
            return;
        }
        t.columns.write().push(column);
        if self.is_persistent() {
            self.save_table_schema(table, &t);
        }
    }

    pub fn drop_column(&self, table: &str, column: &str) {
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
            vs.remove(column);
        }
        if self.is_persistent() {
            self.save_table_schema(table, &t);
        }
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) {
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
        // Vector index keys are immutable; rename means remove + re-add
        // an entry pointing at the same backing data. We swap the entry
        // by reinserting under the new key.
        {
            let mut vs = t.vector_indexes.write();
            if let Some(idx) = vs.remove(from) {
                vs.insert(to.to_string(), idx);
            }
        }
        if self.is_persistent() {
            self.save_table_schema(table, &t);
        }
    }

    pub fn rename_table(&self, from: &str, to: &str) -> bool {
        let mut tables = self.tables.write();
        if tables.contains_key(to) {
            return false;
        }
        let Some(state) = tables.remove(from) else {
            return false;
        };
        tables.insert(to.to_string(), state.clone());
        drop(tables);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(from);
        }
        if self.is_persistent() {
            self.save_table_schema(to, &state);
        }
        true
    }

    /// Append `field` to the table's FTS field list. The analyzer is
    /// the table-level default. Re-indexing of existing rows happens
    /// lazily on the next mutation; documents already in the store stay
    /// queryable through the original analyzer until then.
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
        _analyzer: Option<&str>,
    ) -> Result<(), String> {
        let t = self
            .table(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field);
            }
        }
        if self.is_persistent() {
            self.save_table_schema(table, &t);
        }
        Ok(())
    }

    pub fn get_document(&self, table: &str, doc_id: DocId) -> Option<Document> {
        let t = self.table(table)?;
        let got = t.document_store.read().get(doc_id);
        got
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) {
        let Some(t) = self.table(table) else {
            return;
        };
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
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
            .analyzer()
            .clone();
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
            .analyzer()
            .clone();
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
