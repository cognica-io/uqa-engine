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
    /// `text_match(field, query_string)` — Bayesian BM25 retrieval.
    TextMatch,
    /// `bayesian_match(field, query_string)` — alias, same as
    /// `text_match` for now (Phase 5 ships only Bayesian BM25).
    BayesianMatch,
    /// `knn_match(field, query_vector, k)` — top-k cosine KNN.
    KNNMatch,
    /// `fuse_log_odds(signal_1, signal_2, ...)` — log-odds fusion of
    /// other UQA function calls.
    FuseLogOdds,
    /// `graph_pagerank(graph_name)` — `PageRank` over a named graph.
    GraphPagerank,
    /// `graph_traverse(graph_name, start_vertex, label, max_hops)` —
    /// BFS traversal scoring.
    GraphTraverse,
    /// `graph_neighbors(graph_name, vertex_id, label, direction)` —
    /// 1-hop neighbor expansion.
    GraphNeighbors,
    /// `multi_field_match(field_1, query_1, field_2, query_2, ...)` —
    /// per-field BM25 with uniform-weight log-odds conjunction.
    MultiFieldMatch,
    /// `staged_retrieval(field_1, query_1, top_k_1, field_2, query_2,
    /// top_k_2, ...)` — cascading `text_match`: each stage filters the
    /// candidate set from the previous stage and keeps top-k.
    StagedRetrieval,
    /// `deep_predict(model_name)` — runs the saved deep-fusion model.
    DeepPredict,
}

fn registry() -> &'static BTreeMap<&'static str, FunctionKind> {
    static R: OnceLock<BTreeMap<&'static str, FunctionKind>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m = BTreeMap::new();
        m.insert("text_match", FunctionKind::TextMatch);
        m.insert("bayesian_match", FunctionKind::BayesianMatch);
        m.insert("knn_match", FunctionKind::KNNMatch);
        m.insert("fuse_log_odds", FunctionKind::FuseLogOdds);
        m.insert("graph_pagerank", FunctionKind::GraphPagerank);
        m.insert("graph_traverse", FunctionKind::GraphTraverse);
        m.insert("graph_neighbors", FunctionKind::GraphNeighbors);
        m.insert("multi_field_match", FunctionKind::MultiFieldMatch);
        m.insert("staged_retrieval", FunctionKind::StagedRetrieval);
        m.insert("deep_predict", FunctionKind::DeepPredict);
        m
    })
}

pub fn lookup(name: &str) -> Option<FunctionKind> {
    registry().get(name.to_ascii_lowercase().as_str()).copied()
}

pub fn is_registered(name: &str) -> bool {
    lookup(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_resolve() {
        assert_eq!(lookup("text_match"), Some(FunctionKind::TextMatch));
        assert_eq!(lookup("KNN_MATCH"), Some(FunctionKind::KNNMatch));
        assert_eq!(lookup("fuse_log_odds"), Some(FunctionKind::FuseLogOdds));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(lookup("does_not_exist"), None);
    }
}
