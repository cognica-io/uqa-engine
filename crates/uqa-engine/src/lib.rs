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
use uqa_sql::SqlError;
use uqa_storage::{
    document_store::Document, Catalog, DocumentStore, InvertedIndex, ManagedConnection,
    MemoryDocumentStore, MemoryInvertedIndex, MemoryVectorIndex, SQLiteDocumentStore,
    SQLiteInvertedIndex, SQLiteVectorIndex, SqliteError, TableSchema, VectorFieldSchema,
    VectorIndex,
};

pub use uqa_sql::{SqlParam, SqlResult};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("SQL error: {0}")]
    Sql(#[from] SqlError),
    #[error("storage error: {0}")]
    Storage(#[from] SqliteError),
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
}

struct TableState {
    document_store: RwLock<Box<dyn DocumentStore>>,
    inverted_index: RwLock<Box<dyn InvertedIndex>>,
    vector_indexes: RwLock<BTreeMap<FieldName, Box<dyn VectorIndex>>>,
    fts_fields: Vec<FieldName>,
    analyzer: Analyzer,
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
        }
    }

    /// SQLite-backed engine. Opens (or creates) the database at `path`,
    /// runs catalog migrations, and rebuilds the in-memory table
    /// registry from the persisted catalog.
    pub fn open(path: &Path) -> Result<Self, SqliteError> {
        let conn = ManagedConnection::open(path)?;
        let catalog = Arc::new(Catalog::open(conn.clone())?);
        let mut engine = Self {
            tables: RwLock::new(BTreeMap::new()),
            catalog: Some(catalog.clone()),
            conn: Some(conn.clone()),
        };
        engine.restore_from_catalog(&catalog, &conn)?;
        Ok(engine)
    }

    fn restore_from_catalog(
        &mut self,
        catalog: &Catalog,
        conn: &ManagedConnection,
    ) -> Result<(), SqliteError> {
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
            let table = TableState {
                document_store: RwLock::new(docs),
                inverted_index: RwLock::new(inv),
                vector_indexes: RwLock::new(vectors),
                fts_fields: schema.fts_fields.clone(),
                analyzer,
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
        let Ok(analyzer_json) = serde_json::to_string(&table.analyzer) else {
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
        let _ = catalog.save_table(&TableSchema {
            name: name.to_string(),
            analyzer_json,
            fts_fields: table.fts_fields.clone(),
            vector_fields,
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
            fts_fields,
            analyzer,
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

    fn table(&self, name: &str) -> Option<Arc<TableState>> {
        self.tables.read().get(name).cloned()
    }

    pub fn add_document(&self, table: &str, doc_id: DocId, document: Document) {
        let Some(t) = self.table(table) else {
            return;
        };
        // Index the FTS fields whose values are strings.
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for name in &t.fts_fields {
            if let Some(Value::Str(s)) = document.get(name) {
                text_fields.insert(name.clone(), s.clone());
            }
        }
        if !text_fields.is_empty() {
            t.inverted_index.write().add_document(doc_id, text_fields);
        }
        t.document_store.write().put(doc_id, document);
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
