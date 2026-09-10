//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical retrieval operator dispatch, graph execution, joins and fusion.

use crate::operator_tree::{OperatorOutput, OperatorTreeDriver};
use crate::parallel::ParallelExecutor;
use crate::query::retrieval::combine_signal_priors;
use crate::ScalarExpr;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{
    DocId, GeneralizedPostingList, Payload, PostingEntry, PostingList, Predicate, ScoredEntry,
    Value,
};
use uqa_operators::{
    BayesianEvidenceFusionOperator, DeepGraphDirection, ExternalPriorMode, GatingSpec,
    MultiStageCutoff, MultiStageEntry, OperatorTree, RobustPositiveEvidencePoolOperator,
    TextScoringMode,
};
use uqa_sql::{ast::ColumnType, SQLError, SQLParam};
use uqa_storage::StorageBackendError;

pub mod context;
mod deep_layers;
mod dispatch;
mod fusion;
mod graph;
mod graph_runtime;
pub mod introspection;
mod joins;
pub mod posting;
mod relation_context;
mod relational;

use context::PhysicalDriverContext;
use deep_layers::{
    deep_runtime_gating, lower_deep_batch_norm, lower_deep_conv, lower_deep_dense,
    lower_deep_dropout, lower_deep_pool,
};
use graph_runtime::{
    graph_pattern_from_ir, parse_rpq, restrict_result_to_source, temporal_filter_from_ir,
    GraphNeighborAccess,
};
use introspection::{
    collect_graph_names, first_structured_field, first_text_signal, require_graph_name,
    require_shared_structured_field, require_shared_vector_field, require_text_field,
    require_vector_field, scored_term_count,
};
use posting::{
    fuse_signal_batches_with, fuse_signals_with, numeric_score, scored_to_posting_list,
    sparse_threshold_inline, static_operator, StaticPostingList,
};

type DriverResult<T> = Result<T, SQLError>;

#[derive(Clone, Copy)]
struct WeightedPathExecution<'a> {
    rpq_source: &'a str,
    start_vertex: u64,
    graph: &'a str,
    weight_property: &'a str,
    default_edge_weight: f64,
    max_hops: usize,
    predicate: &'a uqa_operators::PathWeightPredicate,
    predicate_selectivity: f64,
    score: f64,
}

#[derive(Clone, Copy)]
pub struct HybridJoinFields<'a> {
    pub left_structured: &'a str,
    pub left_vector: &'a str,
    pub right_structured: &'a str,
    pub right_vector: &'a str,
}

#[derive(Clone, Copy)]
struct PositiveEvidencePoolExecution<'a> {
    signals: &'a [OperatorTree],
    alpha: f64,
    gating: &'a GatingSpec,
    weights: Option<&'a [f64]>,
    logit_min: Option<&'a [f64]>,
    logit_max: Option<&'a [f64]>,
    adaptive_weights: bool,
}

fn operator_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}
fn graph_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}

pub struct PhysicalRetrievalDriver<'a> {
    pub context: PhysicalDriverContext<'a>,
    pub table: &'a str,
    signal_table: &'a str,
    pub params: &'a [SQLParam],
    pub parallel: ParallelExecutor,
}

impl<'a> PhysicalRetrievalDriver<'a> {
    pub fn new(
        context: PhysicalDriverContext<'a>,
        table: &'a str,
        signal_table: &'a str,
        params: &'a [SQLParam],
    ) -> Self {
        Self {
            context,
            table,
            signal_table,
            params,
            parallel: ParallelExecutor::default(),
        }
    }

    #[must_use]
    pub fn with_parallel(mut self, parallel: ParallelExecutor) -> Self {
        self.parallel = parallel;
        self
    }

    fn bayesian_params_for(&self, field: &str) -> DriverResult<uqa_scoring::BayesianBM25Params> {
        self.context
            .text
            .bayesian_params_for_relation(self.table, self.signal_table, field)
    }

    fn execute_posting_node(&self, op: &OperatorTree) -> DriverResult<PostingList> {
        match self.execute_node(op)? {
            OperatorOutput::Posting(result) => Ok(result),
            OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
            OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
                "{} produces tuple rows and cannot feed a single-document operator",
                crate::operator_tree::operator_name(op)
            ))),
        }
    }

    fn execute_posting_branches(
        &self,
        branches: &[OperatorTree],
    ) -> DriverResult<Vec<PostingList>> {
        let workers: Vec<_> = branches
            .iter()
            .map(|branch| || self.execute_posting_node(branch))
            .collect();
        self.parallel
            .execute_branches(&workers)
            .into_iter()
            .collect()
    }

    fn execute_output_branches(
        &self,
        branches: &[OperatorTree],
    ) -> DriverResult<Vec<OperatorOutput>> {
        let workers: Vec<_> = branches
            .iter()
            .map(|branch| || self.execute_node(branch))
            .collect();
        self.parallel
            .execute_branches(&workers)
            .into_iter()
            .collect()
    }

    fn execute_term(
        &self,
        query: &str,
        field: Option<&str>,
        scoring: Option<TextScoringMode>,
        top_k: Option<uqa_operators::TextTopKPlan>,
    ) -> DriverResult<PostingList> {
        let scoring = scoring.ok_or_else(|| {
            SQLError::Internal(
                "OperatorTree::Term reached PhysicalRetrievalDriver without bound text scoring"
                    .into(),
            )
        })?;
        if let Some(field) = field {
            self.context
                .text
                .validate_text_search_field(self.table, field)?;
            let mode = match scoring {
                TextScoringMode::BM25 => {
                    uqa_scoring::ScoringMode::BM25(uqa_scoring::BM25Params::default())
                }
                TextScoringMode::BayesianBM25 => {
                    uqa_scoring::ScoringMode::BayesianBM25(self.bayesian_params_for(field)?)
                }
                TextScoringMode::CustomBM25(params) => uqa_scoring::ScoringMode::BM25(params),
                TextScoringMode::CustomBayesianBM25(params) => {
                    uqa_scoring::ScoringMode::BayesianBM25(params)
                }
            };
            return self
                .context
                .text
                .search_leaf(
                    self.table,
                    field,
                    query,
                    &mode,
                    top_k.map_or(usize::MAX, |plan| plan.k),
                    top_k,
                )
                .map(|rows| scored_to_posting_list(&rows));
        }
        if top_k.is_some() {
            return Err(SQLError::Internal(
                "physical text top-k requires one concrete field".into(),
            ));
        }
        if matches!(
            scoring,
            TextScoringMode::CustomBM25(_) | TextScoringMode::CustomBayesianBM25(_)
        ) {
            return Err(SQLError::TypeMismatch(
                "explicit text scoring parameters require one concrete field".into(),
            ));
        }
        let fields = self.context.text.fts_fields_for_table(self.table)?;
        if fields.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "text search: table `{}` has no text-indexed columns",
                self.table
            )));
        }
        let mut by_document = BTreeMap::<DocId, f64>::new();
        for field in fields {
            let mode = match scoring {
                TextScoringMode::BM25 => {
                    uqa_scoring::ScoringMode::BM25(uqa_scoring::BM25Params::default())
                }
                TextScoringMode::BayesianBM25 => {
                    uqa_scoring::ScoringMode::BayesianBM25(self.bayesian_params_for(&field)?)
                }
                TextScoringMode::CustomBM25(_) | TextScoringMode::CustomBayesianBM25(_) => {
                    return Err(SQLError::Internal(
                        "custom all-field scoring passed validation without a concrete field"
                            .into(),
                    ));
                }
            };
            for entry in
                self.context
                    .text
                    .search_leaf(self.table, &field, query, &mode, usize::MAX, None)?
            {
                by_document
                    .entry(entry.doc_id)
                    .and_modify(|score| *score = score.max(entry.score))
                    .or_insert(entry.score);
            }
        }
        Ok(scored_to_posting_list(
            &by_document
                .into_iter()
                .map(|(doc_id, score)| ScoredEntry { doc_id, score })
                .collect::<Vec<_>>(),
        ))
    }

    fn execute_knn(
        &self,
        query_vector: &[f32],
        k: usize,
        field: &str,
    ) -> DriverResult<PostingList> {
        self.require_vector_query(field, query_vector)?;
        self.context
            .vector
            .knn_search_leaf(self.table, field, query_vector, k)
            .map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_filter(
        &self,
        field: &str,
        predicate: &Predicate,
        source: Option<&OperatorTree>,
    ) -> DriverResult<PostingList> {
        self.require_column(field)?;
        // Indexed columns resolve through the value index in
        // O(log n + k); the index refuses predicates it cannot answer
        // with evaluated-scan semantics, so this never changes results.
        if let Some(indexed) = self
            .context
            .indexes
            .value_index_scan(self.table, field, predicate)?
        {
            return match source {
                Some(child) => self
                    .execute_posting_node(child)
                    .map(|posting| posting.merge_intersection_owned(&indexed)),
                None => Ok(indexed),
            };
        }
        let candidates: Vec<DocId> = match source {
            Some(child) => {
                let inner = self.execute_posting_node(child)?;
                inner.entries().iter().map(|e| e.doc_id).collect()
            }
            None => self.context.relations.table_doc_ids(self.table)?,
        };
        let values = self
            .context
            .relations
            .get_document_fields(self.table, &candidates, field)?;
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(candidates.len());
        for doc_id in candidates {
            let Some(value) = values.get(&doc_id) else {
                return Err(SQLError::Internal(format!(
                    "Filter consistency error: candidate {doc_id} is missing from the document-field snapshot for table `{}`",
                    self.table
                )));
            };
            if predicate.evaluate(Some(value)) {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}
