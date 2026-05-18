//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function registry: maps SQL function names called inside `WHERE` /
//! projections to UQA-side semantics (text match, vector knn, hybrid
//! fusion, ...).
//!
//! The registry only **classifies** a function by name; the compiler
//! dispatches the actual operator construction once the call signature
//! is bound to its arguments.

use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// `text_match(field, query_string)` - Bayesian BM25 retrieval.
    TextMatch,
    /// `field @@ query` - full-text query-string parser over text and
    /// vector signals.
    FTSMatch,
    /// `bayesian_match(field, query_string)` - alias, same as
    /// `text_match` for now (Phase 5 ships only Bayesian BM25).
    BayesianMatch,
    /// `bayesian_match_with_prior(field, query, prior_field, mode)` -
    /// Bayesian BM25 adjusted by a document-level external prior.
    BayesianMatchWithPrior,
    /// `knn_match(field, query_vector, k)` - top-k cosine KNN.
    KNNMatch,
    /// `fuse_log_odds(signal_1, signal_2, ...)` - log-odds fusion of
    /// other UQA function calls.
    FuseLogOdds,
    /// `graph_pagerank([graph_name])` - `PageRank` over a named graph.
    GraphPagerank,
    /// `graph_hits([graph_name])` - `HITS` over a named graph.
    GraphHits,
    /// `graph_betweenness([graph_name])` - betweenness centrality over a
    /// named graph.
    GraphBetweenness,
    /// `graph_traverse(graph_name, start_vertex, label, max_hops)` -
    /// BFS traversal scoring.
    GraphTraverse,
    /// `graph_neighbors(graph_name, vertex_id, label, direction)` -
    /// 1-hop neighbor expansion.
    GraphNeighbors,
    /// `multi_field_match(field_1, query_1, field_2, query_2, ...)` -
    /// per-field BM25 with uniform-weight log-odds conjunction.
    MultiFieldMatch,
    /// `staged_retrieval(field_1, query_1, top_k_1, field_2, query_2,
    /// top_k_2, ...)` - cascading `text_match`: each stage filters the
    /// candidate set from the previous stage and keeps top-k.
    StagedRetrieval,
    /// `deep_predict(model_name)` - runs the saved deep-fusion model.
    DeepPredict,
    /// `uqa_highlight(field, query [, start_tag, end_tag, max_fragments,
    /// fragment_size])` - markup search results around matched terms.
    UQAHighlight,
    /// `uqa_facets(field [, field2, ...])` - facet counts over the
    /// posting list, computed against the current row context.
    UQAFacets,
    /// `traverse_match(graph, start, label, max_hops)` - BFS traversal
    /// emitting `(doc_id, score)` weighted by hop distance.
    TraverseMatch,
    /// `temporal_traverse(graph, start, label, max_hops, t_min, t_max)`
    /// - `traverse_match` filtered by edge `valid_from`/`valid_to`.
    TemporalTraverse,
    /// `rpq(expr, start [, graph])` - evaluate a Regular Path Query
    /// (Definition 5.1.2) and emit endpoint vertex ids reachable from
    /// `start` along paths matching `expr`.
    RPQ,
    /// `graph_create(graph_name)` - register a new in-memory graph.
    GraphCreate,
    /// `graph_drop(graph_name)` - drop a registered graph.
    GraphDrop,
    /// `graph_edges(graph_name [, label])` - emit every edge in the
    /// graph as `(source, target, label, weight)` rows.
    GraphEdges,
    /// `attention(signal_1, signal_2, ...)` - multi-signal attention
    /// fusion (single-head).
    AttentionFusion,
    /// `learned_fusion(model, signal_1, ...)` - learned per-feature
    /// weight fusion using a saved `LearnedFusion` model.
    LearnedFusion,
    /// `calibrated_vector_match(field, vector, k [, threshold])` -
    /// KNN with calibrated cosine probabilities (Paper 5).
    CalibratedVectorMatch,
    /// `sparse_threshold(signal, threshold)` - drop scores at or below
    /// the threshold and subtract it from survivors.
    SparseThreshold,
    /// `score_bm25([field,] query)` - projection helper exposing the
    /// current match score.
    ScoreBM25,
    /// `score_bayesian_bm25([field,] query)` - projection helper
    /// exposing the current Bayesian BM25 match score.
    ScoreBayesianBM25,
    /// `deep_learn(model, training_set)` - kick off analytical
    /// training (Paper 4) for the named deep-fusion model.
    DeepLearn,
    /// Deep-fusion construction helpers used inside `deep_learn` /
    /// `deep_predict` argument expressions:
    Convolve,
    Pool,
    Flatten,
    Dense,
    Softmax,
    Layer,
    Model,
}

fn registry() -> &'static BTreeMap<&'static str, FunctionKind> {
    static R: OnceLock<BTreeMap<&'static str, FunctionKind>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m = BTreeMap::new();
        m.insert("text_match", FunctionKind::TextMatch);
        m.insert("fts_match", FunctionKind::FTSMatch);
        m.insert("bayesian_match", FunctionKind::BayesianMatch);
        m.insert(
            "bayesian_match_with_prior",
            FunctionKind::BayesianMatchWithPrior,
        );
        m.insert("knn_match", FunctionKind::KNNMatch);
        m.insert("fuse_log_odds", FunctionKind::FuseLogOdds);
        m.insert("graph_pagerank", FunctionKind::GraphPagerank);
        m.insert("pagerank", FunctionKind::GraphPagerank);
        m.insert("graph_hits", FunctionKind::GraphHits);
        m.insert("hits", FunctionKind::GraphHits);
        m.insert("graph_betweenness", FunctionKind::GraphBetweenness);
        m.insert("betweenness", FunctionKind::GraphBetweenness);
        m.insert("graph_traverse", FunctionKind::GraphTraverse);
        m.insert("graph_neighbors", FunctionKind::GraphNeighbors);
        m.insert("multi_field_match", FunctionKind::MultiFieldMatch);
        m.insert("staged_retrieval", FunctionKind::StagedRetrieval);
        m.insert("deep_predict", FunctionKind::DeepPredict);
        m.insert("uqa_highlight", FunctionKind::UQAHighlight);
        m.insert("uqa_facets", FunctionKind::UQAFacets);
        m.insert("traverse_match", FunctionKind::TraverseMatch);
        m.insert("temporal_traverse", FunctionKind::TemporalTraverse);
        m.insert("rpq", FunctionKind::RPQ);
        m.insert("graph_create", FunctionKind::GraphCreate);
        m.insert("create_graph", FunctionKind::GraphCreate);
        m.insert("graph_drop", FunctionKind::GraphDrop);
        m.insert("drop_graph", FunctionKind::GraphDrop);
        m.insert("graph_edges", FunctionKind::GraphEdges);
        m.insert("attention", FunctionKind::AttentionFusion);
        m.insert("fuse_attention", FunctionKind::AttentionFusion);
        m.insert("fuse_multihead", FunctionKind::AttentionFusion);
        m.insert("learned_fusion", FunctionKind::LearnedFusion);
        m.insert("fuse_learned", FunctionKind::LearnedFusion);
        m.insert(
            "calibrated_vector_match",
            FunctionKind::CalibratedVectorMatch,
        );
        m.insert("sparse_threshold", FunctionKind::SparseThreshold);
        m.insert("score_bm25", FunctionKind::ScoreBM25);
        m.insert("score_bayesian_bm25", FunctionKind::ScoreBayesianBM25);
        m.insert("deep_learn", FunctionKind::DeepLearn);
        m.insert("convolve", FunctionKind::Convolve);
        m.insert("pool", FunctionKind::Pool);
        m.insert("flatten", FunctionKind::Flatten);
        m.insert("dense", FunctionKind::Dense);
        m.insert("softmax", FunctionKind::Softmax);
        m.insert("layer", FunctionKind::Layer);
        m.insert("model", FunctionKind::Model);
        m
    })
}

pub fn lookup(name: &str) -> Option<FunctionKind> {
    registry().get(name.to_ascii_lowercase().as_str()).copied()
}

pub fn is_registered(name: &str) -> bool {
    lookup(name).is_some()
}

/// Sorted list of registered SQL function names. CLI completion and
/// documentation generators should consume this instead of duplicating
/// function names.
pub fn registered_names() -> Vec<&'static str> {
    registry().keys().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_resolve() {
        assert_eq!(lookup("text_match"), Some(FunctionKind::TextMatch));
        assert_eq!(lookup("KNN_MATCH"), Some(FunctionKind::KNNMatch));
        assert_eq!(lookup("fuse_log_odds"), Some(FunctionKind::FuseLogOdds));
        assert!(registered_names().contains(&"deep_predict"));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(lookup("does_not_exist"), None);
    }
}
