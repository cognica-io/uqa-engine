//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry dispatch, retrieval execution and final ranked row limits.

use super::RetrievalQueryContext;
use crate::operator_tree::runtime::{execute_tree, expect_posting_output};
use crate::query::graph_lifecycle::{run_graph_create, run_graph_drop};
use uqa_core::ScoredEntry;
use uqa_sql::{
    registry::{lookup, FunctionKind},
    SQLError, SQLParam, ScalarExpr,
};

impl RetrievalQueryContext<'_> {
    pub fn function(
        &self,
        table: &str,
        signal_table: &str,
        name: &str,
        args: &[ScalarExpr],
        params: &[SQLParam],
        top_k: Option<usize>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
        match kind {
            FunctionKind::GraphCreate => {
                run_graph_create(self.graphs, args, params, self.binding.hook)
            }
            FunctionKind::GraphDrop => run_graph_drop(self.graphs, args, params, self.binding.hook),
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
                let tree = self.binding.lower_function(name, args, params)?;
                let tree = match top_k {
                    Some(k) => self.planner.text_top_k(table, tree, k)?,
                    None => tree,
                };
                let posting = expect_posting_output(
                    execute_tree(&self.trees, table, signal_table, params, &tree)?,
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
}
