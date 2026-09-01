//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bridge between a physical relational predicate and the operator-tree IR.
//!
//! The plan-native optimizer marks supported `QueryBlockPlan` predicates as
//! `OperatorTree` or hybrid access paths. This bridge lowers their
//! [`ScalarExpr`] predicate into boolean, scoring, fusion, filter, and
//! index-scan nodes, runs the 10-pass algebraic / graph-aware /
//! fusion-reordering `QueryOptimizer`, and executes the result through
//! `PlanExecutor`.
//!
//! This module wires the two halves together:
//!
//! 1. [`lower_where`] turns a SQL `ScalarExpr` (the WHERE clause) plus the
//!    target table into an `OperatorTree`. Boolean connectives map onto
//!    `Intersect` / `Union` / `Complement`, scoring / KNN / fusion
//!    function calls map onto the matching `OperatorTree` variants, and
//!    column comparison predicates lower into `Filter` nodes. Expressions
//!    outside that retrieval subset stay in the enclosing relational
//!    `UnifiedPlan` filter node.
//! 2. [`EngineDriver`] implements [`OperatorTreeDriver`] with exhaustive
//!    physical dispatch for every concrete IR variant. Ordinary nodes use
//!    `PostingList`, graph nodes retain `GraphPostingList`, and joins retain
//!    their tuple identity in `GeneralizedPostingList`.
//!
//! The integration target is a "lower -> optimise -> execute" pipeline:
//! [`run_optimised`] does the three-step sequence and returns a
//! [`Vec<ScoredEntry>`] that the caller can project, sort, and limit
//! through the relational plan's projection, ordering, and limit nodes.
//! Lowering is selective: when a predicate is not a posting-list access path
//! (for example arithmetic across columns), `None` tells the same relational
//! filter node to evaluate its scalar expression. Once a concrete tree exists,
//! the optimizer and driver execute it or return a typed error.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{
    DocId, GeneralizedPostingList, PathSegment, Payload, PostingEntry, PostingList, Predicate,
    Value,
};
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_operators::{
    BayesianEvidenceFusionOperator, DeepGraphDirection, ExternalPriorMode, GatingSpec,
    MultiStageCutoff, MultiStageEntry, OperatorTree, RobustPositiveEvidencePoolOperator,
    TextScoringMode,
};
use uqa_planner::executor::{OperatorOutput, OperatorTreeDriver, PlanExecutor};
use uqa_planner::parallel::ParallelExecutor;
use uqa_planner::query_optimizer::{IndexScanCandidate, QueryOptimizer};
use uqa_sql::ast::{BinaryOp, ColumnType};
use uqa_sql::SQLParam;
use uqa_storage::StorageBackendError;

use crate::sql;
use crate::{Engine, ScoredEntry};
use uqa_sql::SQLError;

mod deep_layers;
mod driver_context;
mod driver_dispatch;
mod driver_fusion;
mod driver_graph;
mod driver_joins;
mod driver_relational;
mod graph_runtime;
mod lowering_boolean;
mod lowering_constants;
mod lowering_fusion;
mod lowering_graph;
mod lowering_retrieval;
mod operator_join_estimation;
mod operator_join_execution;
mod optimizer_binding;
mod posting_utils;
mod tree_introspection;

use deep_layers::{
    deep_runtime_gating, lower_deep_batch_norm, lower_deep_conv, lower_deep_dense,
    lower_deep_dropout, lower_deep_pool,
};
use graph_runtime::{
    graph_pattern_from_ir, parse_rpq, restrict_result_to_source, temporal_filter_from_ir,
    GraphNeighborSnapshot,
};
use lowering_boolean::{column_name, lower_comparison, lower_document_boolean, lower_function};
use lowering_constants::{
    const_bool, const_f64, const_f64_vector, const_gating, const_optional_string, const_string,
    const_temporal_bound, const_usize, const_value, const_vector, named_arg_expr,
};
use lowering_fusion::{
    lower_bayesian_evidence_fusion, lower_learned_fusion, lower_positive_evidence_pool,
    try_lower_attention_fusion,
};
use lowering_graph::{default_operator_graph, lower_graph_function};
use lowering_retrieval::{
    bind_operator_argument, checked_retrieval_call_tree_present, lower_bayesian_match_with_prior,
    lower_calibrated_vector_match, lower_multi_field_match, lower_operator_arg, lower_signal_arg,
    lower_staged_retrieval, try_lower_fts_match, try_lower_knn_match, try_lower_text_match,
    validate_checked_retrieval_call_tree, validate_operator_function_arity,
    validate_probability_signal_contract,
};
pub(crate) use operator_join_estimation::estimate_operator_join_table_function;
pub(crate) use operator_join_execution::execute_operator_join_table_function;
use optimizer_binding::{engine_query_optimizer, operator_tree_paradigm, scored_term_count};
use posting_utils::{
    fuse_signal_batches_with, fuse_signals_with, numeric_score, posting_list_to_scored,
    scored_to_posting_list, sparse_threshold_inline, static_operator, StaticPostingList,
};
use tree_introspection::{
    collect_graph_names, first_structured_field, first_text_signal, require_graph_name,
    require_shared_structured_field, require_shared_vector_field, require_text_field,
    require_vector_field,
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
struct HybridJoinFields<'a> {
    left_structured: &'a str,
    left_vector: &'a str,
    right_structured: &'a str,
    right_vector: &'a str,
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

enum OptionalStringConstant {
    Null,
    Value(String),
}

impl OptionalStringConstant {
    fn into_option(self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Value(value) => Some(value),
        }
    }
}

fn operator_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}

fn graph_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}

/// Lower a SQL `WHERE` expression into an [`OperatorTree`]. Returns
/// `None` for shapes the operator IR can't represent so the caller can
/// fall back to the row-evaluator path.
pub fn lower_where(expr: &ScalarExpr, params: &[SQLParam]) -> Option<OperatorTree> {
    match expr {
        ScalarExpr::And(parts) => {
            let mut out: Vec<OperatorTree> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, params)?);
            }
            Some(lower_document_boolean(out, false))
        }
        ScalarExpr::Or(parts) => {
            let mut out: Vec<OperatorTree> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, params)?);
            }
            Some(lower_document_boolean(out, true))
        }
        // Complement is only sound when the inner predicate cannot be
        // NULL for any row (search functions, IS NULL tests). Column
        // comparisons under NOT fall through to the wildcard `None`
        // and keep three-valued semantics through the row-evaluator
        // relational evaluation: `NOT (col = 5)` must not match rows whose `col`
        // is NULL.
        ScalarExpr::Not(inner) if crate::sql::expr_is_null_free_public(inner) => Some(
            OperatorTree::Complement(Box::new(lower_where(inner, params)?)),
        ),
        ScalarExpr::Func { name, args, .. } => lower_function(name, args, params),
        ScalarExpr::Binary { op, lhs, rhs } => lower_comparison(*op, lhs, rhs, params),
        ScalarExpr::IsNull { expr, negated } => {
            let field = column_name(expr)?;
            let predicate = if *negated {
                Predicate::IsNotNull
            } else {
                Predicate::IsNull
            };
            Some(OperatorTree::Filter {
                field,
                predicate,
                source: None,
            })
        }
        ScalarExpr::Between { expr, low, high } => {
            let field = column_name(expr)?;
            let lo = const_value(low, params)?;
            let hi = const_value(high, params)?;
            Some(OperatorTree::Filter {
                field,
                predicate: Predicate::Between { low: lo, high: hi },
                source: None,
            })
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let field = column_name(expr)?;
            let mut set: BTreeSet<Value> = BTreeSet::new();
            let mut has_null = false;
            for v in list {
                let value = const_value(v, params)?;
                if matches!(value, Value::Null) {
                    has_null = true;
                    continue;
                }
                set.insert(value);
            }
            if *negated {
                // `col NOT IN (...)`: a NULL in the list means no row
                // can ever satisfy it; otherwise complement the match
                // set but keep NULL rows excluded (three-valued NOT).
                if has_null {
                    return Some(OperatorTree::Empty);
                }
                let filter = OperatorTree::Filter {
                    field: field.clone(),
                    predicate: Predicate::InSet(set),
                    source: None,
                };
                let not_null = OperatorTree::Filter {
                    field,
                    predicate: Predicate::IsNotNull,
                    source: None,
                };
                return Some(OperatorTree::Intersect(vec![
                    OperatorTree::Complement(Box::new(filter)),
                    not_null,
                ]));
            }
            Some(OperatorTree::Filter {
                field,
                predicate: Predicate::InSet(set),
                source: None,
            })
        }
        _ => None,
    }
}

/// Bind runtime scalar arguments, then require a concrete operator node.
///
/// The optimizer-side lowerer is intentionally pure and therefore only
/// folds literals and SQL parameters.  Row-emitting calls can also contain
/// deterministic scalar expressions (for example a concatenated query
/// string).  Evaluate those expressions once at the physical boundary and
/// retry the same lowerer.  A registered retrieval function must never fall
/// through to a second row-function implementation merely because one of its
/// arguments needed runtime binding.
pub(crate) fn lower_sql_function_bound(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    validate_operator_function_arity(name, args.len())?;
    validate_probability_signal_contract(name, args)?;
    let mut bound = args
        .iter()
        .map(|argument| bind_operator_argument(engine, argument, params))
        .collect::<Result<Vec<_>, _>>()?;
    match name.to_ascii_lowercase().as_str() {
        "rpq" if bound.len() == 2 => bound.push(ScalarExpr::Literal(Value::Str(
            default_operator_graph(engine, "rpq")?,
        ))),
        "graph_pagerank" | "pagerank" | "graph_hits" | "hits" | "graph_betweenness"
        | "betweenness"
            if bound.is_empty() =>
        {
            bound.push(ScalarExpr::Literal(Value::Str(default_operator_graph(
                engine, name,
            )?)));
        }
        _ => {}
    }
    validate_checked_retrieval_call_tree(name, &bound, &[])?;
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "attention" | "fuse_attention" | "fuse_multihead"
    ) {
        return try_lower_attention_fusion(name, &bound, &[]);
    }
    lower_function(name, &bound, &[]).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{name} arguments cannot be lowered to the shared operator IR"
        ))
    })
}

/// Physical `OperatorTreeDriver` backed by the engine's table, index, graph,
/// join, and ML runtimes. Single-document branches compose through the core
/// document support operations and documented payload merge policies; join
/// branches retain the generalized tuple carrier.
#[derive(Clone, Copy)]
enum DriverExecution {
    Public,
    InExecution,
}

pub struct EngineDriver<'a> {
    pub engine: &'a Engine,
    pub table: &'a str,
    pub params: &'a [SQLParam],
    pub parallel: ParallelExecutor,
    execution: DriverExecution,
}

impl<'a> EngineDriver<'a> {
    #[must_use]
    pub fn new(engine: &'a Engine, table: &'a str, params: &'a [SQLParam]) -> EngineDriver<'a> {
        Self {
            engine,
            table,
            params,
            parallel: ParallelExecutor::default(),
            execution: DriverExecution::Public,
        }
    }

    fn new_in_execution(
        engine: &'a Engine,
        table: &'a str,
        params: &'a [SQLParam],
    ) -> EngineDriver<'a> {
        Self {
            engine,
            table,
            params,
            parallel: ParallelExecutor::default(),
            execution: DriverExecution::InExecution,
        }
    }

    /// Override the branch-level parallel executor. The default uses
    /// rayon's pool with `DEFAULT_PARALLEL_WORKERS`; pass `0` for
    /// fully-serial execution in tests / deterministic benchmarks.
    #[must_use]
    pub fn with_parallel(mut self, par: ParallelExecutor) -> Self {
        self.parallel = par;
        self
    }

    fn bayesian_params_for(&self, field: &str) -> DriverResult<uqa_scoring::BayesianBM25Params> {
        match self.execution {
            DriverExecution::Public => self.engine.bayesian_params_for(self.table, field),
            DriverExecution::InExecution => self
                .engine
                .bayesian_params_for_in_execution(self.table, field),
        }
    }

    fn execute_posting_node(&self, op: &OperatorTree) -> DriverResult<PostingList> {
        match self.execute_node(op)? {
            OperatorOutput::Posting(result) => Ok(result),
            OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
            OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
                "{} produces tuple rows and cannot feed a single-document operator",
                uqa_planner::executor::operator_name(op)
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
                "OperatorTree::Term reached EngineDriver without bound text scoring".into(),
            )
        })?;
        if let Some(field) = field {
            self.engine.validate_text_search_field(self.table, field)?;
            let mode = match scoring {
                TextScoringMode::BM25 => crate::ScoringMode::BM25(crate::BM25Params::default()),
                TextScoringMode::BayesianBM25 => {
                    crate::ScoringMode::BayesianBM25(self.bayesian_params_for(field)?)
                }
                TextScoringMode::CustomBM25(params) => crate::ScoringMode::BM25(params),
                TextScoringMode::CustomBayesianBM25(params) => {
                    crate::ScoringMode::BayesianBM25(params)
                }
            };
            return self
                .engine
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
        let fields = self.engine.fts_fields_for_table(self.table)?;
        if fields.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "text search: table `{}` has no text-indexed columns",
                self.table
            )));
        }
        let mut by_document = BTreeMap::<DocId, f64>::new();
        for field in fields {
            let mode = match scoring {
                TextScoringMode::BM25 => crate::ScoringMode::BM25(crate::BM25Params::default()),
                TextScoringMode::BayesianBM25 => {
                    crate::ScoringMode::BayesianBM25(self.bayesian_params_for(&field)?)
                }
                TextScoringMode::CustomBM25(_) | TextScoringMode::CustomBayesianBM25(_) => {
                    return Err(SQLError::Internal(
                        "custom all-field scoring passed validation without a concrete field"
                            .into(),
                    ));
                }
            };
            for entry in
                self.engine
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
        self.engine
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
        if let Some(indexed) = self.engine.value_index_scan(self.table, field, predicate)? {
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
            None => self.engine.table_doc_ids(self.table)?,
        };
        let values = self
            .engine
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

/// Lower a WHERE expression and run [`QueryOptimizer`] over the
/// resulting tree without executing it. Useful for tests and
/// `EXPLAIN`-style diagnostics that want to inspect the rewritten
/// shape before any posting list is materialised.
pub fn optimised_tree_for(
    engine: &Engine,
    table: &str,
    where_expr: &ScalarExpr,
    params: &[SQLParam],
) -> DriverResult<Option<OperatorTree>> {
    let Some(tree) = lower_where_bound(engine, where_expr, params)? else {
        return Ok(None);
    };
    Ok(Some(
        engine_query_optimizer(engine, table, &tree)?.optimize(tree),
    ))
}

/// Cost a relation-local SQL predicate through the same lowering and
/// optimizer configuration used by execution.
pub(crate) fn estimate_local_access(
    engine: &Engine,
    table: &str,
    where_expr: &ScalarExpr,
    params: &[SQLParam],
) -> DriverResult<Option<uqa_planner::LocalAccessEstimate>> {
    let Some(tree) = lower_where_bound(engine, where_expr, params)? else {
        return Ok(None);
    };
    estimate_operator_tree_access(engine, table, tree, true).map(Some)
}

fn estimate_operator_tree_access(
    engine: &Engine,
    table: &str,
    tree: OperatorTree,
    clamp_to_table: bool,
) -> DriverResult<uqa_planner::LocalAccessEstimate> {
    let optimizer = engine_query_optimizer(engine, table, &tree)?;
    let planned_tree = optimizer.optimize(tree);
    let total_docs = optimizer.index_stats.total_docs as f64;
    let output_rows = optimizer
        .estimator
        .estimate(&planned_tree, &optimizer.index_stats);
    if !output_rows.is_finite() || output_rows < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cardinality {output_rows}"
        )));
    }
    let output_rows = if clamp_to_table {
        output_rows.min(total_docs)
    } else {
        output_rows
    };
    let cost = optimizer
        .cost_model
        .estimate(&planned_tree, &optimizer.index_stats);
    if !cost.is_finite() || cost < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cost {cost}"
        )));
    }
    Ok(uqa_planner::LocalAccessEstimate {
        output_rows,
        cost,
        paradigm: operator_tree_paradigm(&planned_tree),
    })
}

pub(crate) fn is_operator_join_table_function(name: &str) -> bool {
    uqa_sql::registry::is_operator_join_table_function(name)
}

fn lower_join_operand(
    engine: &Engine,
    expression: &ScalarExpr,
    params: &[SQLParam],
    function_name: &str,
) -> DriverResult<OperatorTree> {
    lower_where_bound(engine, expression, params)?.ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{function_name} operand cannot be represented by the operator IR"
        ))
    })
}

fn const_join_threshold(
    expression: &ScalarExpr,
    params: &[SQLParam],
    function_name: &str,
    minimum: f64,
    maximum: f64,
) -> DriverResult<f64> {
    let threshold = const_f64(expression, params).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{function_name}.threshold must be a constant number"
        ))
    })?;
    if !threshold.is_finite() || !(minimum..=maximum).contains(&threshold) {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.threshold must be finite and in [{minimum}, {maximum}], got {threshold}"
        )));
    }
    Ok(threshold)
}

fn lower_operator_join_table_function(
    engine: &Engine,
    name: &str,
    relations: Option<&uqa_sql::ast::OperatorJoinRelations>,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<(uqa_sql::ast::OperatorJoinRelations, OperatorTree)> {
    let expected = match name {
        "text_similarity_join" | "vector_similarity_join" => 5,
        "graph_join" => 6,
        "hybrid_join" | "cross_paradigm_join" => 4,
        _ => {
            return Err(SQLError::Unsupported(format!(
                "operator join table function `{name}`"
            )))
        }
    };
    let relations = relations.ok_or_else(|| {
        SQLError::TypeMismatch(format!("{name} requires left and right table identifiers"))
    })?;
    let actual = args.len() + 2;
    if actual != expected {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    let left = lower_join_operand(engine, &args[0], params, name)?;
    let right = lower_join_operand(engine, &args[1], params, name)?;
    let tree = match name {
        "text_similarity_join" => OperatorTree::TextSimilarityJoin {
            left: Box::new(left),
            right: Box::new(right),
            threshold: const_join_threshold(&args[2], params, "text_similarity_join", 0.0, 1.0)?,
        },
        "vector_similarity_join" => OperatorTree::VectorSimilarityJoin {
            left: Box::new(left),
            right: Box::new(right),
            threshold: const_join_threshold(&args[2], params, "vector_similarity_join", -1.0, 1.0)?,
        },
        "graph_join" => OperatorTree::GraphJoin {
            left: Box::new(left),
            right: Box::new(right),
            label: const_optional_string(&args[2], params)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "graph_join.label must be a constant string or NULL".into(),
                    )
                })?
                .into_option(),
            graph: const_string(&args[3], params).ok_or_else(|| {
                SQLError::TypeMismatch("graph_join.graph must be a constant string".into())
            })?,
        },
        "hybrid_join" => OperatorTree::HybridJoin {
            left: Box::new(left),
            right: Box::new(right),
        },
        "cross_paradigm_join" => OperatorTree::CrossParadigmJoin {
            left: Box::new(left),
            right: Box::new(right),
        },
        _ => unreachable!("operator join name validated above"),
    };
    Ok((relations.clone(), tree))
}

fn centrality_kind(name: &str) -> Option<&'static str> {
    match name {
        "graph_pagerank" | "pagerank" => Some("pagerank"),
        "graph_hits" | "hits" => Some("hits"),
        "graph_betweenness" | "betweenness" => Some("betweenness"),
        _ => None,
    }
}

fn lower_bound_centrality(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    kind: &str,
) -> DriverResult<OperatorTree> {
    let graph = match args {
        [] => default_operator_graph(engine, name)?,
        [_] => {
            return Err(SQLError::TypeMismatch(format!(
                "{name}.graph must be a constant string"
            )))
        }
        _ => {
            return Err(SQLError::BadArity {
                name: name.to_string(),
                expected: "0..=1".into(),
                actual: args.len(),
            })
        }
    };
    Ok(match kind {
        "pagerank" => OperatorTree::PageRank { graph },
        "hits" => OperatorTree::HITS { graph },
        _ => OperatorTree::BetweennessCentrality { graph },
    })
}

fn lower_bound_rpq(
    engine: &Engine,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<OperatorTree> {
    let graph = default_operator_graph(engine, "rpq")?;
    let rpq_source = const_string(&args[0], params)
        .ok_or_else(|| SQLError::TypeMismatch("rpq.expr must be a constant string".into()))?;
    let start_vertex = const_usize(&args[1], params)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| SQLError::TypeMismatch("rpq.start must be a non-negative integer".into()))?;
    Ok(OperatorTree::RegularPathQuery {
        rpq_source,
        start_vertex,
        graph,
    })
}

fn lower_bound_function(
    engine: &Engine,
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<Option<OperatorTree>> {
    validate_operator_function_arity(name, args.len())?;
    validate_probability_signal_contract(name, args)?;

    let bound;
    let (lowering_args, lowering_params): (&[ScalarExpr], &[SQLParam]) =
        if checked_retrieval_call_tree_present(name, args) {
            bound = args
                .iter()
                .map(|argument| bind_operator_argument(engine, argument, params))
                .collect::<Result<Vec<_>, _>>()?;
            (&bound, &[])
        } else {
            (args, params)
        };
    validate_checked_retrieval_call_tree(name, lowering_args, lowering_params)?;

    if let Some(tree) = lower_function(name, lowering_args, lowering_params) {
        return Ok(Some(tree));
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "attention" | "fuse_attention" | "fuse_multihead"
    ) {
        return try_lower_attention_fusion(name, lowering_args, lowering_params).map(Some);
    }
    let lower_name = name.to_ascii_lowercase();
    if let Some(kind) = centrality_kind(&lower_name) {
        return lower_bound_centrality(engine, name, lowering_args, kind).map(Some);
    }
    if lower_name == "rpq" && lowering_args.len() == 2 {
        return lower_bound_rpq(engine, lowering_args, lowering_params).map(Some);
    }
    if matches!(
        lower_name.as_str(),
        "graph_traverse"
            | "traverse_match"
            | "graph_neighbors"
            | "graph_edges"
            | "temporal_traverse"
            | "rpq"
            | "deep_predict"
    ) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} arguments must be execution-time constants of the documented types"
        )));
    }
    Ok(None)
}

fn lower_where_bound(
    engine: &Engine,
    expression: &ScalarExpr,
    params: &[SQLParam],
) -> Result<Option<OperatorTree>, SQLError> {
    match expression {
        ScalarExpr::And(parts) => {
            let mut children = Vec::with_capacity(parts.len());
            for part in parts {
                let Some(child) = lower_where_bound(engine, part, params)? else {
                    return Ok(None);
                };
                children.push(child);
            }
            Ok(Some(lower_document_boolean(children, false)))
        }
        ScalarExpr::Or(parts) => {
            let mut children = Vec::with_capacity(parts.len());
            for part in parts {
                let Some(child) = lower_where_bound(engine, part, params)? else {
                    return Ok(None);
                };
                children.push(child);
            }
            Ok(Some(lower_document_boolean(children, true)))
        }
        ScalarExpr::Not(inner) if crate::sql::expr_is_null_free_public(inner) => {
            Ok(lower_where_bound(engine, inner, params)?
                .map(|child| OperatorTree::Complement(Box::new(child))))
        }
        ScalarExpr::Func { name, args, .. } => lower_bound_function(engine, name, args, params),
        _ => Ok(lower_where(expression, params)),
    }
}

pub(crate) enum DirectVectorRetrieval {
    Knn {
        top_k: usize,
    },
    Calibrated {
        field: String,
        query_vector: Vec<f32>,
        top_k: usize,
        threshold: Option<f64>,
    },
}

/// Describe a complete predicate that owns one bounded vector candidate pool.
/// A hierarchy scan applies that pool and any query-local calibration once
/// after merging every physical relation.
mod execution;
pub use execution::run_optimised;
pub(crate) use execution::{
    combine_signal_priors, direct_vector_retrieval, execute_operator_tree_in_execution,
    execute_scored_tree, expect_posting_output, run_accelerated,
};
