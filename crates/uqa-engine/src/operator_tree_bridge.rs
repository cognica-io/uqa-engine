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
//! 2. The execution crate's physical retrieval driver exhaustively dispatches concrete IR variants. The public [`EngineDriver`] binds that executor to the active statement and transaction; graph and tuple carriers retain their distinct identities.
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

use uqa_core::{PathSegment, Predicate, Value};
use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_execution::parallel::ParallelExecutor;
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_operators::{
    DeepGraphDirection, ExternalPriorMode, GatingSpec, MultiStageCutoff, MultiStageEntry,
    OperatorTree, TextScoringMode,
};
use uqa_planner::query_optimizer::{IndexScanCandidate, QueryOptimizer};
use uqa_sql::ast::BinaryOp;
use uqa_sql::SQLParam;

use crate::{Engine, ScoredEntry};
use uqa_sql::SQLError;

mod lowering_boolean;
mod lowering_constants;
mod lowering_fusion;
mod lowering_graph;
mod lowering_retrieval;
mod operator_join_estimation;
mod optimizer_binding;

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
pub(crate) use optimizer_binding::engine_query_optimizer;
use optimizer_binding::operator_tree_paradigm;
type DriverResult<T> = Result<T, SQLError>;

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

/// Bind a physical retrieval driver to the Engine statement and transaction boundary.
pub struct EngineDriver<'a> {
    pub engine: &'a Engine,
    pub table: &'a str,
    pub params: &'a [SQLParam],
    pub parallel: ParallelExecutor,
}

impl<'a> EngineDriver<'a> {
    #[must_use]
    pub fn new(engine: &'a Engine, table: &'a str, params: &'a [SQLParam]) -> Self {
        Self {
            engine,
            table,
            params,
            parallel: ParallelExecutor::default(),
        }
    }
    #[must_use]
    pub fn with_parallel(mut self, parallel: ParallelExecutor) -> Self {
        self.parallel = parallel;
        self
    }
}

impl OperatorTreeDriver for EngineDriver<'_> {
    type Error = SQLError;
    fn execute_node(&self, tree: &OperatorTree) -> DriverResult<OperatorOutput> {
        execution::execute_public_physical_node(self, tree)
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

pub(crate) fn lower_operator_join_table_function(
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

pub(crate) use uqa_execution::query::table_sources::retrieval::DirectVectorRetrieval;

/// Describe a complete predicate that owns one bounded vector candidate pool.
/// A hierarchy scan applies that pool and any query-local calibration once
/// after merging every physical relation.
mod execution;
pub use execution::run_optimised;
pub(crate) use execution::{
    direct_vector_retrieval, execute_relation_operator_tree_in_execution, execute_scored_tree,
    expect_posting_output, run_accelerated,
};

use uqa_execution::operator_tree::driver::introspection::collect_graph_names;
use uqa_execution::operator_tree::driver::posting::posting_list_to_scored;
