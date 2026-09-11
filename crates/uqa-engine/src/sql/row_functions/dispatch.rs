//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function-registry dispatch, operator lowering, and top-K planning.

use super::{lookup, Engine, FunctionKind, SQLError, SQLParam, ScalarExpr, ScoredEntry};

use uqa_execution::query::graph_lifecycle::{run_graph_create, run_graph_drop};

pub(in crate::sql) fn execute_function_with_top_k(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
    top_k: Option<usize>,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::GraphCreate => run_graph_create(engine, args, params, engine),
        FunctionKind::GraphDrop => run_graph_drop(engine, args, params, engine),
        FunctionKind::GraphExists
        | FunctionKind::GraphLabelCreate
        | FunctionKind::GraphLabelDrop
        | FunctionKind::GraphAlter
        | FunctionKind::UQAHighlight
        | FunctionKind::UQAFacets
        | FunctionKind::ScoreBM25
        | FunctionKind::ScoreBayesianBM25
        | FunctionKind::DeepLearn
        | FunctionKind::Convolve
        | FunctionKind::Pool
        | FunctionKind::Flatten
        | FunctionKind::Dense
        | FunctionKind::Softmax
        | FunctionKind::Layer
        | FunctionKind::Model => Err(SQLError::Unsupported(format!(
            "row-emitting dispatch for `{name}` is handled elsewhere"
        ))),
        FunctionKind::TextMatch
        | FunctionKind::BayesianMatch
        | FunctionKind::FTSMatch
        | FunctionKind::BayesianMatchWithPrior
        | FunctionKind::SparseThreshold
        | FunctionKind::KNNMatch
        | FunctionKind::CalibratedVectorMatch
        | FunctionKind::FuseLogOdds
        | FunctionKind::PositiveEvidencePool
        | FunctionKind::BayesianEvidenceFusion
        | FunctionKind::GraphPagerank
        | FunctionKind::GraphHits
        | FunctionKind::GraphBetweenness
        | FunctionKind::GraphTraverse
        | FunctionKind::GraphNeighbors
        | FunctionKind::MultiFieldMatch
        | FunctionKind::StagedRetrieval
        | FunctionKind::DeepPredict
        | FunctionKind::TraverseMatch
        | FunctionKind::TemporalTraverse
        | FunctionKind::RPQ
        | FunctionKind::GraphEdges
        | FunctionKind::AttentionFusion
        | FunctionKind::LearnedFusion => {
            let tree =
                crate::operator_tree_bridge::lower_sql_function_bound(engine, name, args, params)?;
            let tree = match top_k {
                Some(k) => plan_bound_text_top_k(engine, table, tree, k)?,
                None => tree,
            };
            let posting = crate::operator_tree_bridge::expect_posting_output(
                crate::operator_tree_bridge::execute_relation_operator_tree_in_execution(
                    engine,
                    table,
                    signal_table,
                    params,
                    &tree,
                )?,
                name,
            )?;
            let posting = match top_k {
                Some(k) => posting.ranked().select_top_k(k),
                None => posting,
            };
            Ok(posting
                .entries()
                .iter()
                .map(|entry| ScoredEntry {
                    doc_id: entry.doc_id,
                    score: entry.payload.score,
                })
                .collect())
        }
    }
}

fn plan_bound_text_top_k(
    engine: &Engine,
    table: &str,
    tree: uqa_operators::OperatorTree,
    top_k: usize,
) -> Result<uqa_operators::OperatorTree, SQLError> {
    let (query, field, scoring) = match tree {
        uqa_operators::OperatorTree::Term {
            query,
            field: Some(field),
            scoring: Some(scoring),
            top_k: None,
        } => (query, field, scoring),
        other => return Ok(other),
    };
    engine.plan_text_top_k_tree(table, &field, &query, scoring, top_k)
}

impl uqa_execution::query::block::context::RelationRetrieval for Engine {
    fn accelerated(
        &self,
        table: &str,
        signal_table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        crate::operator_tree_bridge::run_accelerated(self, table, signal_table, predicate, params)
    }
    fn optimized(
        &self,
        table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        crate::operator_tree_bridge::run_optimised(self, table, predicate, params)
    }
    fn function(
        &self,
        table: &str,
        signal_table: &str,
        name: &str,
        args: &[ScalarExpr],
        params: &[SQLParam],
        top_k: Option<usize>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        execute_function_with_top_k(self, table, signal_table, name, args, params, top_k)
    }
}
