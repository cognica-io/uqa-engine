//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-paradigm operators (Sections 3.1-3.3, Paper 2).
//!
//! These bridge the document, vector, and graph algebras: build a
//! graph from a document corpus or from a token-co-occurrence
//! analysis, score graph traversal results by vector similarity, or
//! re-rank pattern matches against a query embedding. `FromGraph` is
//! just [`GraphPostingList::to_posting_list`] — call it directly.

use std::collections::BTreeMap;

use uqa_analysis::{analyzer::standard_analyzer, AnalysisError};
use uqa_core::{Edge, Payload, PostingEntry, PostingList, Value, Vertex, VertexId};

use crate::memory_store::MemoryGraphStore;
use crate::operators::{GMatch, Traverse};
use crate::pattern::GraphPattern;
use crate::posting_list::{GraphPayload, GraphPostingList, GraphPostingListError};
use crate::store::{GraphStore, GraphStoreError};

#[derive(Debug, thiserror::Error)]
pub enum CrossParadigmError {
    #[error(transparent)]
    Analysis(#[from] AnalysisError),
    #[error(transparent)]
    GraphStore(#[from] GraphStoreError),
    #[error(transparent)]
    InvalidPostingList(#[from] GraphPostingListError),
    #[error("invalid cross-paradigm input: {0}")]
    InvalidInput(String),
    #[error("cross-paradigm arithmetic overflow: {0}")]
    ArithmeticOverflow(String),
}

pub type CrossParadigmResult<T> = Result<T, CrossParadigmError>;

/// A simple document representation for `ToGraph` / `TextToGraph`.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub doc_id: VertexId,
    pub fields: BTreeMap<String, Value>,
}

impl Document {
    pub fn new(doc_id: VertexId) -> Self {
        Self {
            doc_id,
            fields: BTreeMap::new(),
        }
    }
}

/// Convert a document corpus into a fresh `MemoryGraphStore`.
///
/// One vertex per document under graph `default`, one directed
/// `link`-labeled edge for each id listed under `edge_field`. Document
/// fields outside `edge_field` are copied into the vertex's
/// `properties`.
pub struct ToGraph {
    pub documents: Vec<Document>,
    pub edge_field: String,
}

impl ToGraph {
    pub fn new(documents: Vec<Document>) -> Self {
        Self {
            documents,
            edge_field: "links".into(),
        }
    }

    pub fn edge_field(mut self, name: impl Into<String>) -> Self {
        self.edge_field = name.into();
        self
    }

    pub fn execute(self) -> CrossParadigmResult<MemoryGraphStore> {
        let mut graph = MemoryGraphStore::new();
        graph.create_graph("default");
        for doc in &self.documents {
            let mut props: BTreeMap<String, Value> = doc.fields.clone();
            props.remove(&self.edge_field);
            graph.add_vertex(
                Vertex {
                    vertex_id: doc.doc_id,
                    label: String::new(),
                    properties: props,
                },
                "default",
            )?;
        }
        let mut edge_counter = 1u64;
        for doc in &self.documents {
            let Some(targets) = doc.fields.get(&self.edge_field) else {
                continue;
            };
            let Value::List(items) = targets else {
                return Err(CrossParadigmError::InvalidInput(format!(
                    "document {} field {:?} must be a list of integer vertex ids",
                    doc.doc_id, self.edge_field
                )));
            };
            for target in items {
                let Value::Int(target_id) = target else {
                    return Err(CrossParadigmError::InvalidInput(format!(
                        "document {} field {:?} contains a non-integer vertex id",
                        doc.doc_id, self.edge_field
                    )));
                };
                let target_id = VertexId::try_from(*target_id).map_err(|_| {
                    CrossParadigmError::InvalidInput(format!(
                        "document {} field {:?} contains negative vertex id {target_id}",
                        doc.doc_id, self.edge_field
                    ))
                })?;
                graph.add_edge(
                    Edge::new(edge_counter, doc.doc_id, target_id, "link"),
                    "default",
                )?;
                edge_counter = edge_counter.checked_add(1).ok_or_else(|| {
                    CrossParadigmError::ArithmeticOverflow(
                        "document link edge id counter overflow".to_string(),
                    )
                })?;
            }
        }
        Ok(graph)
    }
}

/// Build a token co-occurrence graph from a document corpus.
///
/// Each unique token becomes a vertex (label `""`, property `token`).
/// `window_size == 0` connects every pair of distinct tokens that
/// appear in the same document; a positive `window_size` only connects
/// tokens within `window_size` positions of each other. Edges are
/// labeled `co_occurs` and carry a `weight` property equal to the
/// total co-occurrence count.
pub struct TextToGraph {
    pub documents: Vec<Document>,
    pub text_field: String,
    pub window_size: usize,
    pub language: String,
}

impl TextToGraph {
    pub fn new(documents: Vec<Document>) -> Self {
        Self {
            documents,
            text_field: "text".into(),
            window_size: 0,
            language: "english".into(),
        }
    }

    pub fn text_field(mut self, name: impl Into<String>) -> Self {
        self.text_field = name.into();
        self
    }

    pub fn window_size(mut self, w: usize) -> Self {
        self.window_size = w;
        self
    }

    pub fn language(mut self, lang: impl Into<String>) -> Self {
        self.language = lang.into();
        self
    }

    pub fn execute(self) -> CrossParadigmResult<MemoryGraphStore> {
        let analyzer = standard_analyzer(&self.language);
        let mut token_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut cooccurrences: BTreeMap<(String, String), u64> = BTreeMap::new();

        for doc in &self.documents {
            let text = match doc.fields.get(&self.text_field) {
                Some(Value::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let tokens = analyzer.analyze(&text)?;
            for token in &tokens {
                token_set.insert(token.clone());
            }
            if self.window_size == 0 {
                let mut unique: Vec<String> = tokens.clone();
                unique.sort();
                unique.dedup();
                for i in 0..unique.len() {
                    for j in (i + 1)..unique.len() {
                        let pair = (unique[i].clone(), unique[j].clone());
                        increment_cooccurrence(&mut cooccurrences, pair)?;
                    }
                }
            } else {
                for i in 0..tokens.len() {
                    let end = i
                        .saturating_add(self.window_size)
                        .saturating_add(1)
                        .min(tokens.len());
                    for j in (i + 1)..end {
                        if tokens[i] == tokens[j] {
                            continue;
                        }
                        let (a, b) = if tokens[i] < tokens[j] {
                            (tokens[i].clone(), tokens[j].clone())
                        } else {
                            (tokens[j].clone(), tokens[i].clone())
                        };
                        increment_cooccurrence(&mut cooccurrences, (a, b))?;
                    }
                }
            }
        }

        let mut graph = MemoryGraphStore::new();
        graph.create_graph("default");
        let mut token_to_id: BTreeMap<String, VertexId> = BTreeMap::new();
        for (idx, token) in token_set.iter().enumerate() {
            let vid = VertexId::try_from(idx)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    CrossParadigmError::ArithmeticOverflow(
                        "token vertex id counter overflow".to_string(),
                    )
                })?;
            token_to_id.insert(token.clone(), vid);
            let mut props = BTreeMap::new();
            props.insert("token".into(), Value::Str(token.clone()));
            graph.add_vertex(
                Vertex {
                    vertex_id: vid,
                    label: String::new(),
                    properties: props,
                },
                "default",
            )?;
        }
        for (index, ((t1, t2), weight)) in cooccurrences.into_iter().enumerate() {
            let edge_counter = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    CrossParadigmError::ArithmeticOverflow(
                        "co-occurrence edge id counter overflow".to_string(),
                    )
                })?;
            let src = token_to_id.get(&t1).copied().ok_or_else(|| {
                CrossParadigmError::InvalidInput(format!("missing token vertex for {t1:?}"))
            })?;
            let tgt = token_to_id.get(&t2).copied().ok_or_else(|| {
                CrossParadigmError::InvalidInput(format!("missing token vertex for {t2:?}"))
            })?;
            let mut edge = Edge::new(edge_counter, src, tgt, "co_occurs");
            let weight = i64::try_from(weight).map_err(|_| {
                CrossParadigmError::ArithmeticOverflow(format!(
                    "co-occurrence weight {weight} exceeds i64"
                ))
            })?;
            edge.properties.insert("weight".into(), Value::Int(weight));
            graph.add_edge(edge, "default")?;
        }
        Ok(graph)
    }
}

/// Per-vertex cosine similarity to a query embedding. Reads the named
/// vector property and emits a standard `PostingList` keyed by vertex
/// id, with the cosine score on the payload.
pub struct VertexEmbedding<'a> {
    pub graph: &'a str,
    pub query_vector: Vec<f64>,
    pub vector_field: String,
    pub threshold: f64,
}

impl<'a> VertexEmbedding<'a> {
    pub fn new(graph: &'a str, query_vector: Vec<f64>) -> Self {
        Self {
            graph,
            query_vector,
            vector_field: "embedding".into(),
            threshold: 0.0,
        }
    }

    pub fn vector_field(mut self, name: impl Into<String>) -> Self {
        self.vector_field = name.into();
        self
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> CrossParadigmResult<PostingList> {
        validate_vector_query(&self.query_vector, self.threshold)?;
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut ids: Vec<VertexId> = store.vertex_ids_in_graph(self.graph)?.into_iter().collect();
        ids.sort_unstable();
        for vid in ids {
            let Some(vertex) = store.get_vertex(vid) else {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph {:?} references missing vertex {vid}",
                    self.graph
                ))
                .into());
            };
            let Some(vec) = read_vector(&vertex.properties, &self.vector_field)? else {
                continue;
            };
            let sim = cosine_similarity(&self.query_vector, &vec)?;
            if sim >= self.threshold {
                entries.push(PostingEntry::new(vid, Payload::with_score(sim)));
            }
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}

/// `Traverse` followed by a vector-similarity filter: keep only the
/// vertices whose `vector_field` similarity to `query_vector` clears
/// `threshold`. The retained entries' scores are the cosine values.
pub struct SemanticGraphSearch<'a> {
    pub graph: &'a str,
    pub start_vertex: VertexId,
    pub label: Option<&'a str>,
    pub max_hops: u32,
    pub query_vector: Vec<f64>,
    pub vector_field: String,
    pub threshold: f64,
}

impl<'a> SemanticGraphSearch<'a> {
    pub fn new(graph: &'a str, start_vertex: VertexId, query_vector: Vec<f64>) -> Self {
        Self {
            graph,
            start_vertex,
            label: None,
            max_hops: 1,
            query_vector,
            vector_field: "embedding".into(),
            threshold: 0.5,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn max_hops(mut self, hops: u32) -> Self {
        self.max_hops = hops;
        self
    }

    pub fn vector_field(mut self, name: impl Into<String>) -> Self {
        self.vector_field = name.into();
        self
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> CrossParadigmResult<GraphPostingList> {
        validate_vector_query(&self.query_vector, self.threshold)?;
        let mut traverse = Traverse::new(self.start_vertex, self.graph).max_hops(self.max_hops);
        if let Some(l) = self.label {
            traverse = traverse.label(l);
        }
        let gpl = traverse.execute(store)?;
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<VertexId, GraphPayload> = BTreeMap::new();
        for entry in gpl.inner().entries() {
            let Some(vertex) = store.get_vertex(entry.doc_id) else {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "traversal returned missing vertex {}",
                    entry.doc_id
                ))
                .into());
            };
            let Some(vec) = read_vector(&vertex.properties, &self.vector_field)? else {
                continue;
            };
            let sim = cosine_similarity(&self.query_vector, &vec)?;
            if sim < self.threshold {
                continue;
            }
            entries.push(PostingEntry::new(entry.doc_id, Payload::with_score(sim)));
            if let Some(gp) = gpl.get_graph_payload(entry.doc_id) {
                let mut copy = gp.clone();
                copy.score_override = Some(sim);
                graph_payloads.insert(entry.doc_id, copy);
            }
        }
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }
}

/// `GMatch` followed by a per-match cosine filter against the vertex
/// bound to `score_variable`. Matches with similarity below
/// `threshold` are dropped; surviving entries take the cosine value as
/// their score.
pub struct VectorEnhancedMatch<'a> {
    pub graph: &'a str,
    pub pattern: GraphPattern,
    pub query_vector: Vec<f64>,
    pub score_variable: String,
    pub vector_field: String,
    pub threshold: f64,
}

impl<'a> VectorEnhancedMatch<'a> {
    pub fn new(
        graph: &'a str,
        pattern: GraphPattern,
        query_vector: Vec<f64>,
        score_variable: impl Into<String>,
    ) -> Self {
        Self {
            graph,
            pattern,
            query_vector,
            score_variable: score_variable.into(),
            vector_field: "embedding".into(),
            threshold: 0.0,
        }
    }

    pub fn vector_field(mut self, name: impl Into<String>) -> Self {
        self.vector_field = name.into();
        self
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> CrossParadigmResult<GraphPostingList> {
        validate_vector_query(&self.query_vector, self.threshold)?;
        let match_op = GMatch::new(self.pattern.clone(), self.graph);
        let result = match_op.execute(store)?;
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<VertexId, GraphPayload> = BTreeMap::new();
        for entry in result.inner().entries() {
            let Some(Value::Int(vid_i)) = entry.payload.fields.get(&self.score_variable) else {
                continue;
            };
            let vid = VertexId::try_from(*vid_i).map_err(|_| {
                CrossParadigmError::InvalidInput(format!(
                    "match variable {:?} contains invalid vertex id {vid_i}",
                    self.score_variable
                ))
            })?;
            let Some(vertex) = store.get_vertex(vid) else {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "match variable {:?} references missing vertex {vid}",
                    self.score_variable
                ))
                .into());
            };
            let Some(vec) = read_vector(&vertex.properties, &self.vector_field)? else {
                continue;
            };
            let sim = cosine_similarity(&self.query_vector, &vec)?;
            if sim < self.threshold {
                continue;
            }
            entries.push(PostingEntry::new(
                entry.doc_id,
                Payload {
                    positions: Vec::new(),
                    score: sim,
                    fields: entry.payload.fields.clone(),
                },
            ));
            if let Some(gp) = result.get_graph_payload(entry.doc_id) {
                let mut copy = gp.clone();
                copy.score_override = Some(sim);
                graph_payloads.insert(entry.doc_id, copy);
            }
        }
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }
}

fn increment_cooccurrence(
    cooccurrences: &mut BTreeMap<(String, String), u64>,
    pair: (String, String),
) -> CrossParadigmResult<()> {
    let count = cooccurrences.entry(pair).or_insert(0);
    *count = count.checked_add(1).ok_or_else(|| {
        CrossParadigmError::ArithmeticOverflow("co-occurrence counter overflow".to_string())
    })?;
    Ok(())
}

fn validate_vector_query(query: &[f64], threshold: f64) -> CrossParadigmResult<()> {
    if query.is_empty() {
        return Err(CrossParadigmError::InvalidInput(
            "query vector must not be empty".to_string(),
        ));
    }
    if query.iter().any(|value| !value.is_finite()) || !threshold.is_finite() {
        return Err(CrossParadigmError::InvalidInput(
            "query vector and threshold must be finite".to_string(),
        ));
    }
    Ok(())
}

fn read_vector(
    properties: &BTreeMap<String, Value>,
    field: &str,
) -> CrossParadigmResult<Option<Vec<f64>>> {
    let Some(value) = properties.get(field) else {
        return Ok(None);
    };
    let Value::List(items) = value else {
        return Err(CrossParadigmError::InvalidInput(format!(
            "vector field {field:?} must be a list"
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Float(f) if f.is_finite() => out.push(*f),
            Value::Int(n) if n.unsigned_abs() <= (1_u64 << 53) => out.push(*n as f64),
            Value::Int(n) => {
                return Err(CrossParadigmError::InvalidInput(format!(
                    "integer vector component {n} cannot be represented exactly as f64"
                )));
            }
            _ => {
                return Err(CrossParadigmError::InvalidInput(format!(
                    "vector field {field:?} contains a non-numeric or non-finite component"
                )));
            }
        }
    }
    Ok(Some(out))
}

fn cosine_similarity(a: &[f64], b: &[f64]) -> CrossParadigmResult<f64> {
    if a.len() != b.len() || a.is_empty() {
        return Err(CrossParadigmError::InvalidInput(format!(
            "cosine vectors must have the same non-zero dimension ({} != {})",
            a.len(),
            b.len()
        )));
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return Err(CrossParadigmError::InvalidInput(
            "cosine vectors must have non-zero norm".to_string(),
        ));
    }
    let similarity = dot / (na.sqrt() * nb.sqrt());
    if !similarity.is_finite() {
        return Err(CrossParadigmError::InvalidInput(
            "cosine similarity is non-finite".to_string(),
        ));
    }
    Ok(similarity)
}
