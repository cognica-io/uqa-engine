//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level engine: a per-table [`DocumentStore`] + [`InvertedIndex`]
//! pair, document mutation entry points, and a minimal `search` API for
//! text-only round trips. Catalog restore, schemas, and the SQL frontend
//! are added in subsequent phases.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;
use uqa_analysis::{analyzer::standard_analyzer, Analyzer};
use uqa_core::{DocId, FieldName, PostingEntry, Value};
use uqa_operators::{ExecutionContext, Operator, ScoreOperator, TermOperator};
use uqa_scoring::{BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, Scorer};
use uqa_storage::{
    document_store::Document, DocumentStore, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex,
};

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

/// In-memory engine: one [`MemoryDocumentStore`] and one
/// [`MemoryInvertedIndex`] per registered table. The pair is held under a
/// shared [`RwLock`] so mutations from `add_document` and reads from
/// `search` interleave safely. Multi-table layout, persistence, and the
/// schema-aware namespace land later.
pub struct Engine {
    tables: RwLock<BTreeMap<String, Arc<TableState>>>,
}

struct TableState {
    document_store: RwLock<MemoryDocumentStore>,
    inverted_index: RwLock<MemoryInvertedIndex>,
    fts_fields: Vec<FieldName>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(BTreeMap::new()),
        }
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
        let table = TableState {
            document_store: RwLock::new(MemoryDocumentStore::new()),
            inverted_index: RwLock::new(MemoryInvertedIndex::new(analyzer)),
            fts_fields,
        };
        self.tables.write().insert(name, Arc::new(table));
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
        let cloned = t.document_store.read().get(doc_id).cloned();
        cloned
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) {
        let Some(t) = self.table(table) else {
            return;
        };
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
    }

    pub fn document_count(&self, table: &str) -> u64 {
        self.table(table)
            .map_or(0, |t| t.inverted_index.read().doc_count())
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
        let Some(t) = self.table(table) else {
            return Vec::new();
        };

        // Snapshot the inverted index and document store so the operator
        // pipeline runs without holding write locks. The clones are
        // Arc-internal handles, not deep copies of the data.
        let idx_snapshot: Arc<dyn InvertedIndex> = Arc::new(t.inverted_index.read().clone());
        let stats = idx_snapshot.stats();
        let analyzer = idx_snapshot.analyzer().clone();
        let stats_arc = Arc::new(stats.clone());

        let ctx = ExecutionContext::new()
            .with_inverted_index(idx_snapshot.clone())
            .with_document_store({
                let ds_clone = t.document_store.read().clone();
                Arc::new(ds_clone) as Arc<dyn DocumentStore>
            })
            .with_stats(stats);

        let term_op: Arc<dyn Operator> = Arc::new(TermOperator::new(query, field));

        let scorer: Arc<dyn Scorer> = match mode {
            ScoringMode::BM25(p) => Arc::new(BM25Scorer::new(*p, stats_arc.clone())),
            ScoringMode::BayesianBM25(p) => {
                Arc::new(BayesianBM25Scorer::new(*p, stats_arc.clone()))
            }
        };

        let analyzed_terms = analyzer.analyze(query);
        if analyzed_terms.is_empty() {
            return Vec::new();
        }

        let score_op = ScoreOperator::new(scorer, term_op, analyzed_terms, field);
        let result = score_op.execute(&ctx);

        let mut entries: Vec<ScoredEntry> = result.iter().map(ScoredEntry::from_entry).collect();
        entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        entries.truncate(top_k);
        entries
    }
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
    fn document_count_tracks_indexed_documents() {
        let eng = Engine::new();
        eng.create_default_table("articles", vec!["title".into()]);
        for i in 0..5 {
            eng.add_document("articles", i, doc([("title", s(&format!("doc {i}")))]));
        }
        assert_eq!(eng.document_count("articles"), 5);
    }
}
