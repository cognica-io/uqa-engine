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
//!    physical dispatch for every concrete IR variant. Ordinary and graph
//!    nodes use `PostingList` (with graph Phi payloads); joins retain their
//!    tuple identity in `GeneralizedPostingList`.
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
    GatingSpec, LogOddsFusionOperator, MultiStageCutoff, MultiStageEntry, OperatorTree,
    TextScoringMode,
};
use uqa_planner::executor::{OperatorOutput, OperatorTreeDriver, PlanExecutor};
use uqa_planner::parallel::ParallelExecutor;
use uqa_planner::query_optimizer::{IndexScanCandidate, QueryOptimizer};
use uqa_sql::ast::{BinaryOp, ColumnType};
use uqa_sql::expr::value_to_vector;
use uqa_sql::SQLParam;

use crate::sql;
use crate::{Engine, ScoredEntry};
use uqa_sql::SQLError;

type DriverResult<T> = Result<T, SQLError>;
type ScoredFusionSignal = (Vec<ScoredEntry>, Option<f64>);

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

fn malformed_operator_meta(operator: &str, field: &str) -> SQLError {
    SQLError::Internal(format!(
        "malformed {operator} OperatorTree: missing or invalid `{field}` metadata"
    ))
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
            Some(OperatorTree::Intersect(out))
        }
        ScalarExpr::Or(parts) => {
            let mut out: Vec<OperatorTree> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, params)?);
            }
            Some(OperatorTree::Union(out))
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

fn lower_function(name: &str, args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "text_match" => {
            let field = column_name(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            Some(OperatorTree::Term {
                query,
                field: Some(field),
                scoring: Some(TextScoringMode::BM25),
            })
        }
        "bayesian_match" => {
            let field = column_name(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            Some(OperatorTree::Term {
                query,
                field: Some(field),
                scoring: Some(TextScoringMode::BayesianBM25),
            })
        }
        "fts_match" => {
            let default_field = fts_default_field(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            compile_fts_query(&query, default_field.as_deref()).map(prepare_fts_probability_tree)
        }
        "bayesian_match_with_prior" => lower_bayesian_match_with_prior(args, params),
        "calibrated_vector_match" => lower_calibrated_vector_match(args, params),
        "knn_match" => {
            // Standalone knn_match preserves raw cosine similarities;
            // calibration to (0, 1) only fires inside fusion contexts.
            // (mirrors `_compile_calibrated_signal` semantics from the
            // canonical UQA behavior: only fusion arms see calibrated KNN).
            let field = column_name(args.first()?)?;
            let vec_expr = args.get(1)?;
            let query_vector = const_vector(vec_expr, params)?;
            let k = const_usize(args.get(2)?, params)?;
            Some(OperatorTree::KNN {
                query_vector,
                k,
                field,
            })
        }
        "fuse_log_odds" => lower_fuse_log_odds(args, params),
        "multi_field_match" => lower_multi_field_match(args, params),
        "staged_retrieval" => lower_staged_retrieval(args, params),
        "attention" | "fuse_attention" | "fuse_multihead" => lower_attention_fusion(args, params),
        "learned_fusion" | "fuse_learned" => lower_learned_fusion(args, params),
        "sparse_threshold" => {
            if args.len() != 2 {
                return None;
            }
            let source = lower_signal_arg(args.first()?, params)?;
            let threshold = const_f64(args.get(1)?, params)?;
            Some(OperatorTree::SparseThreshold {
                source: Box::new(source),
                threshold,
            })
        }
        _ => None,
    }
}

/// Lower one row-emitting SQL function call into the shared operator
/// IR. The row-function physical node uses this for non-WHERE call
/// sites so compositional retrieval functions do not retain duplicate
/// physical implementations.
pub(crate) fn lower_sql_function(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    lower_function(name, args, params)
}

fn lower_bayesian_match_with_prior(
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    if args.len() != 4 {
        return None;
    }
    let mut meta = BTreeMap::new();
    meta.insert("field".to_string(), Value::Str(column_name(args.first()?)?));
    meta.insert(
        "query".to_string(),
        Value::Str(const_string(args.get(1)?, params)?),
    );
    meta.insert(
        "prior_field".to_string(),
        Value::Str(column_name(args.get(2)?)?),
    );
    let mode = const_string(args.get(3)?, params)?;
    if !matches!(mode.to_ascii_lowercase().as_str(), "authority" | "recency") {
        return None;
    }
    meta.insert("mode".to_string(), Value::Str(mode));
    Some(OperatorTree::Opaque {
        kind: "bayesian_match_with_prior".to_string(),
        children: Vec::new(),
        meta,
    })
}

fn lower_calibrated_vector_match(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    if !(3..=4).contains(&args.len()) {
        return None;
    }
    let mut meta = BTreeMap::new();
    meta.insert(
        "field".to_string(),
        Value::Str(field_name_arg(args.first()?, params)?),
    );
    meta.insert(
        "query_vector".to_string(),
        Value::List(
            const_vector(args.get(1)?, params)?
                .into_iter()
                .map(|v| Value::Float(f64::from(v)))
                .collect(),
        ),
    );
    meta.insert(
        "k".to_string(),
        Value::Int(const_usize(args.get(2)?, params)? as i64),
    );
    if args.len() == 4 {
        let threshold = const_f64(args.get(3)?, params)?;
        meta.insert("threshold".to_string(), Value::Float(threshold));
    }
    Some(OperatorTree::Opaque {
        kind: "calibrated_vector_match".to_string(),
        children: Vec::new(),
        meta,
    })
}

fn lower_multi_field_match(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    if args.len() < 3 {
        return None;
    }
    let first_non_column = args.iter().position(|arg| column_name(arg).is_none());
    if let Some(query_idx) = first_non_column {
        if query_idx >= 2 {
            let fields = args[..query_idx]
                .iter()
                .map(column_name)
                .collect::<Option<Vec<_>>>()?;
            let query = const_string(args.get(query_idx)?, params)?;
            let weight_args = &args[query_idx + 1..];
            let weights = if weight_args.is_empty() {
                None
            } else {
                if weight_args.len() != fields.len() {
                    return None;
                }
                Some(
                    weight_args
                        .iter()
                        .map(|arg| const_f64(arg, params))
                        .collect::<Option<Vec<_>>>()?,
                )
            };
            return Some(OperatorTree::MultiFieldSearch {
                fields,
                query,
                weights,
            });
        }
    }

    if args.len() < 4 || args.len() % 2 != 0 {
        return None;
    }
    let n_fields = args.len() / 2;
    let mut fields = Vec::with_capacity(n_fields);
    let mut queries = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        fields.push(column_name(&args[2 * i])?);
        queries.push(const_string(&args[2 * i + 1], params)?);
    }
    let first_query = queries.first()?.clone();
    if queries.iter().all(|query| query == &first_query) {
        return Some(OperatorTree::MultiFieldSearch {
            fields,
            query: first_query,
            weights: None,
        });
    }
    None
}

fn lower_staged_retrieval(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    let mut stages = Vec::new();
    if matches!(args.first(), Some(ScalarExpr::Func { .. }))
        && named_arg_expr(args.first()?).is_none()
    {
        if args.is_empty() || args.len() % 2 != 0 {
            return None;
        }
        for pair in args.chunks(2) {
            stages.push(MultiStageEntry {
                child: lower_signal_arg(&pair[0], params)?,
                cutoff: MultiStageCutoff::TopK(const_usize(&pair[1], params)?),
            });
        }
    } else {
        if args.is_empty() || args.len() % 3 != 0 {
            return None;
        }
        for stage in args.chunks(3) {
            stages.push(MultiStageEntry {
                child: OperatorTree::Term {
                    query: const_string(&stage[1], params)?,
                    field: Some(column_name(&stage[0])?),
                    scoring: Some(TextScoringMode::BM25),
                },
                cutoff: MultiStageCutoff::TopK(const_usize(&stage[2], params)?),
            });
        }
    }
    (!stages.is_empty()).then_some(OperatorTree::MultiStage { stages })
}

/// Compile a signal-function call into a node that produces calibrated
/// probabilities in (0, 1). Mirrors the canonical UQA implementation's
/// `_compile_calibrated_signal`: in fusion contexts every signal must
/// land on the (0, 1) probability scale before log-odds / attention /
/// learned fusion can combine them.
///
/// - `bayesian_match` --> [`OperatorTree::Term`] with Bayesian BM25 scoring.
/// - `fts_match` text trees --> [`OperatorTree::BayesianScore`] around the
///   complete raw BM25 Boolean query.
/// - `knn_match` --> [`OperatorTree::CosineProbability`] wrapping a
///   [`OperatorTree::KNN`] child, so cosine scores in `[-1, 1]` get
///   rescaled to `(0, 1)` via `(1 + s) / 2`.
fn lower_calibrated_signal(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    match name {
        "bayesian_match" => {
            let field = column_name(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            Some(OperatorTree::Term {
                query,
                field: Some(field),
                scoring: Some(TextScoringMode::BayesianBM25),
            })
        }
        "fts_match" => {
            let default_field = fts_default_field(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            compile_fts_query(&query, default_field.as_deref()).map(prepare_fts_probability_tree)
        }
        "bayesian_match_with_prior" => lower_bayesian_match_with_prior(args, params),
        "knn_match" => {
            let field = column_name(args.first()?)?;
            let vec_expr = args.get(1)?;
            let query_vector = const_vector(vec_expr, params)?;
            let k = const_usize(args.get(2)?, params)?;
            Some(OperatorTree::CosineProbability(Box::new(
                OperatorTree::KNN {
                    query_vector,
                    k,
                    field,
                },
            )))
        }
        "calibrated_vector_match" => lower_calibrated_vector_match(args, params),
        _ => None,
    }
}

/// Lower a function-call argument into a calibrated signal node. Used
/// by every fusion lowering arm (`fuse_log_odds`, `attention`,
/// `learned_fusion`) so the rewrite stays consistent across fusers.
fn lower_signal_arg(arg: &ScalarExpr, params: &[SQLParam]) -> Option<OperatorTree> {
    match arg {
        ScalarExpr::Func { name, args, .. } => {
            let lower = name.to_ascii_lowercase();
            lower_calibrated_signal(&lower, args, params)
        }
        _ => None,
    }
}

fn lower_fuse_log_odds(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    // `fuse_log_odds(signal_1, signal_2, ...[, alpha[, gating]])`.
    // The UQA SQL contract defaults alpha to 0.5 when no numeric option is supplied;
    // don't treat the last signal as an alpha argument.
    if args.len() < 2 {
        return None;
    }

    let mut alpha = 0.5;
    let mut gating = GatingSpec::Softplus;
    let mut weights = None;
    let mut logit_min = None;
    let mut logit_max = None;
    let mut signal_end = args.len();
    while signal_end > 0 {
        let option = &args[signal_end - 1];
        if let Some((name, value_expr)) = named_arg_expr(option) {
            if name.eq_ignore_ascii_case("alpha") {
                alpha = const_f64(value_expr, params)?;
            } else if name.eq_ignore_ascii_case("gating") {
                gating = const_gating(value_expr, params)?;
            } else if name.eq_ignore_ascii_case("weights") {
                weights = Some(const_f64_vector(value_expr, params)?);
            } else if name.eq_ignore_ascii_case("logit_min") {
                logit_min = Some(const_f64_vector(value_expr, params)?);
            } else if name.eq_ignore_ascii_case("logit_max") {
                logit_max = Some(const_f64_vector(value_expr, params)?);
            } else {
                return None;
            }
            signal_end -= 1;
            continue;
        }
        if let Some(g) = const_gating(option, params) {
            gating = g;
            signal_end -= 1;
            continue;
        }
        if let Some(v) = const_f64(option, params) {
            alpha = v;
            signal_end -= 1;
            continue;
        }
        break;
    }
    if signal_end < 2 {
        return None;
    }
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return None;
    }
    if let Some(signal_weights) = &weights {
        let sum = signal_weights.iter().sum::<f64>();
        if signal_weights.len() != signal_end
            || signal_weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight < 0.0)
            || (sum - 1.0).abs() > 1e-3
        {
            return None;
        }
    }
    match (&logit_min, &logit_max) {
        (Some(minimums), Some(maximums))
            if minimums.len() == signal_end && maximums.len() == signal_end => {}
        (Some(_), Some(_)) => return None,
        _ => {
            logit_min = None;
            logit_max = None;
        }
    }

    let mut signals: Vec<OperatorTree> = Vec::with_capacity(signal_end);
    for a in &args[..signal_end] {
        signals.push(lower_signal_arg(a, params)?);
    }
    Some(OperatorTree::LogOddsFusion {
        signals,
        alpha,
        gating,
        weights,
        logit_min,
        logit_max,
    })
}

fn lower_attention_fusion(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    use std::sync::Arc;
    use uqa_fusion::{AttentionFusion, N_QUERY_FEATURES};
    use uqa_operators::tree::AttentionRef;

    let mut signals: Vec<OperatorTree> = Vec::new();
    for a in args {
        signals.push(lower_signal_arg(a, params)?);
    }
    if signals.len() < 2 {
        return None;
    }
    // `attention(signal_1, signal_2, ...)` defaults: alpha=0.5,
    // n_query_features=6 (matches UQA behavior `_make_attention_fusion_op`).
    // Query features are filled in lazily at execute time from the
    // engine snapshot, so the IR carries an empty explicit vector.
    let attention: AttentionRef =
        Arc::new(AttentionFusion::new(signals.len(), N_QUERY_FEATURES, 0.5));
    Some(OperatorTree::AttentionFusion {
        signals,
        attention,
        query_features: Vec::new(),
    })
}

fn lower_learned_fusion(args: &[ScalarExpr], params: &[SQLParam]) -> Option<OperatorTree> {
    use std::sync::Arc;
    use uqa_fusion::LearnedFusion;
    use uqa_operators::tree::LearnedFusionRef;

    let mut signals: Vec<OperatorTree> = Vec::new();
    for a in args {
        signals.push(lower_signal_arg(a, params)?);
    }
    if signals.len() < 2 {
        return None;
    }
    let learned: LearnedFusionRef = Arc::new(LearnedFusion::new(signals.len(), 0.5));
    Some(OperatorTree::LearnedFusion { signals, learned })
}

fn lower_comparison(
    op: BinaryOp,
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    params: &[SQLParam],
) -> Option<OperatorTree> {
    // Allow either `col OP literal` or `literal OP col` (we normalise).
    let (col_expr, val_expr, swap) = match (column_name(lhs), column_name(rhs)) {
        (Some(_), _) => (lhs, rhs, false),
        (None, Some(_)) => (rhs, lhs, true),
        _ => return None,
    };
    let field = column_name(col_expr)?;
    let value = const_value(val_expr, params)?;
    let predicate = match (op, swap) {
        (BinaryOp::Equal, _) => Predicate::Equals(value),
        (BinaryOp::NotEqual, _) => Predicate::NotEquals(value),
        (BinaryOp::Less, false) | (BinaryOp::Greater, true) => Predicate::LessThan(value),
        (BinaryOp::LessEqual, false) | (BinaryOp::GreaterEqual, true) => {
            Predicate::LessThanOrEqual(value)
        }
        (BinaryOp::Greater, false) | (BinaryOp::Less, true) => Predicate::GreaterThan(value),
        (BinaryOp::GreaterEqual, false) | (BinaryOp::LessEqual, true) => {
            Predicate::GreaterThanOrEqual(value)
        }
        _ => return None,
    };
    Some(OperatorTree::Filter {
        field,
        predicate,
        source: None,
    })
}

fn column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn field_name_arg(expr: &ScalarExpr, params: &[SQLParam]) -> Option<String> {
    column_name(expr).or_else(|| const_string(expr, params))
}

enum FtsDefaultField {
    Field(String),
    All,
}

impl FtsDefaultField {
    fn as_deref(&self) -> Option<&str> {
        match self {
            FtsDefaultField::Field(field) => Some(field),
            FtsDefaultField::All => None,
        }
    }
}

fn fts_default_field(expr: &ScalarExpr) -> Option<FtsDefaultField> {
    match expr {
        ScalarExpr::Column(name) => Some(FtsDefaultField::Field(name.clone())),
        ScalarExpr::QualifiedColumn { column, .. } => Some(FtsDefaultField::Field(column.clone())),
        ScalarExpr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => {
            Some(FtsDefaultField::All)
        }
        _ => None,
    }
}

fn compile_fts_query(query: &str, default_field: Option<&str>) -> Option<OperatorTree> {
    let tokenizer = |_field: Option<&str>, phrase: &str| {
        phrase
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    uqa_sql::compile_fts_query_string(query, default_field, &tokenizer).ok()
}

fn prepare_fts_probability_tree(tree: OperatorTree) -> OperatorTree {
    if is_text_query_tree(&tree) {
        let field = common_text_field(&tree);
        return OperatorTree::BayesianScore {
            source: Box::new(bind_fts_bm25_tree(tree)),
            field,
        };
    }

    match tree {
        OperatorTree::KNN {
            query_vector,
            k,
            field,
        } => OperatorTree::CosineProbability(Box::new(OperatorTree::KNN {
            query_vector,
            k,
            field,
        })),
        OperatorTree::Intersect(children) => OperatorTree::Intersect(
            children
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
        ),
        OperatorTree::Union(children) => OperatorTree::Union(
            children
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
        ),
        OperatorTree::Complement(child) => {
            OperatorTree::Complement(Box::new(prepare_fts_probability_tree(*child)))
        }
        OperatorTree::LogOddsFusion {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
        } => OperatorTree::LogOddsFusion {
            signals: signals
                .into_iter()
                .map(prepare_fts_probability_tree)
                .collect(),
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
        },
        OperatorTree::CosineProbability(child) => OperatorTree::CosineProbability(child),
        other => other,
    }
}

fn is_text_query_tree(tree: &OperatorTree) -> bool {
    match tree {
        OperatorTree::Empty | OperatorTree::Term { .. } => true,
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().all(is_text_query_tree),
        OperatorTree::Complement(child) => is_text_query_tree(child),
        _ => false,
    }
}

fn bind_fts_bm25_tree(tree: OperatorTree) -> OperatorTree {
    match tree {
        OperatorTree::Term { query, field, .. } => OperatorTree::Term {
            query,
            field,
            scoring: Some(TextScoringMode::BM25),
        },
        OperatorTree::Intersect(children) => {
            OperatorTree::Intersect(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Union(children) => {
            OperatorTree::Union(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Composed(children) => {
            OperatorTree::Composed(children.into_iter().map(bind_fts_bm25_tree).collect())
        }
        OperatorTree::Complement(child) => {
            OperatorTree::Complement(Box::new(bind_fts_bm25_tree(*child)))
        }
        other => other,
    }
}

fn common_text_field(tree: &OperatorTree) -> Option<String> {
    fn collect_fields(tree: &OperatorTree, fields: &mut BTreeSet<Option<String>>) {
        match tree {
            OperatorTree::Term { field, .. } => {
                fields.insert(field.clone());
            }
            OperatorTree::Intersect(children)
            | OperatorTree::Union(children)
            | OperatorTree::Composed(children) => {
                for child in children {
                    collect_fields(child, fields);
                }
            }
            OperatorTree::Complement(child) => collect_fields(child, fields),
            _ => {}
        }
    }

    let mut fields = BTreeSet::new();
    collect_fields(tree, &mut fields);
    if fields.len() == 1 {
        fields.into_iter().next().flatten()
    } else {
        None
    }
}

fn const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
}

fn const_string(expr: &ScalarExpr, params: &[SQLParam]) -> Option<String> {
    match const_value(expr, params)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

fn const_f64(expr: &ScalarExpr, params: &[SQLParam]) -> Option<f64> {
    match const_value(expr, params)? {
        Value::Int(n) => Some(n as f64),
        Value::Float(f) => Some(f),
        Value::Decimal(d) => d.to_f64(),
        _ => None,
    }
}

fn const_usize(expr: &ScalarExpr, params: &[SQLParam]) -> Option<usize> {
    match const_value(expr, params)? {
        Value::Int(n) if n >= 0 => Some(n as usize),
        _ => None,
    }
}

fn const_vector(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Vec<f32>> {
    match expr {
        ScalarExpr::Array(items) => {
            let mut out: Vec<f32> = Vec::with_capacity(items.len());
            for v in items {
                out.push(const_f64(v, params)? as f32);
            }
            Some(out)
        }
        other => match const_value(other, params)? {
            Value::List(items) => {
                let mut out: Vec<f32> = Vec::with_capacity(items.len());
                for v in items {
                    match v {
                        Value::Int(n) => out.push(n as f32),
                        Value::Float(f) => out.push(f as f32),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        },
    }
}

fn const_f64_vector(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Vec<f64>> {
    match expr {
        ScalarExpr::Array(items) => items.iter().map(|value| const_f64(value, params)).collect(),
        other => match const_value(other, params)? {
            Value::List(items) => items
                .into_iter()
                .map(|value| match value {
                    Value::Int(number) => Some(number as f64),
                    Value::Float(number) => Some(number),
                    Value::Decimal(number) => number.to_f64(),
                    _ => None,
                })
                .collect(),
            _ => None,
        },
    }
}

fn const_gating(expr: &ScalarExpr, params: &[SQLParam]) -> Option<GatingSpec> {
    match const_value(expr, params)? {
        Value::Str(s) if s.eq_ignore_ascii_case("softplus") => Some(GatingSpec::Softplus),
        Value::Str(s) if s.eq_ignore_ascii_case("pass") || s.eq_ignore_ascii_case("none") => {
            Some(GatingSpec::Pass)
        }
        Value::Str(s) if s.eq_ignore_ascii_case("sigmoid") => Some(GatingSpec::Sigmoid {
            feature: String::new(),
        }),
        Value::Str(s) if s.eq_ignore_ascii_case("relu") => Some(GatingSpec::ReLU),
        Value::Str(s) if s.eq_ignore_ascii_case("swish") => Some(GatingSpec::Swish),
        Value::Str(s) if s.eq_ignore_ascii_case("gelu") => Some(GatingSpec::Gelu),
        _ => None,
    }
}

fn named_arg_expr(expr: &ScalarExpr) -> Option<(&str, &ScalarExpr)> {
    let ScalarExpr::Func { name, args, .. } = expr else {
        return None;
    };
    if name != "__named_arg" || args.len() != 2 {
        return None;
    }
    let ScalarExpr::Literal(Value::Str(arg_name)) = &args[0] else {
        return None;
    };
    Some((arg_name.as_str(), &args[1]))
}

/// Physical `OperatorTreeDriver` backed by the engine's table, index, graph,
/// join, and ML runtimes. Single-document branches compose through the core
/// posting-list algebra; join branches retain the generalized tuple carrier.
pub struct EngineDriver<'a> {
    pub engine: &'a Engine,
    pub table: &'a str,
    pub params: &'a [SQLParam],
    pub parallel: ParallelExecutor,
}

impl<'a> EngineDriver<'a> {
    #[must_use]
    pub fn new(engine: &'a Engine, table: &'a str, params: &'a [SQLParam]) -> EngineDriver<'a> {
        Self {
            engine,
            table,
            params,
            parallel: ParallelExecutor::default(),
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

    fn execute_posting_node(&self, op: &OperatorTree) -> DriverResult<PostingList> {
        match self.execute_node(op)? {
            OperatorOutput::Posting(result) => Ok(result),
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
    ) -> DriverResult<PostingList> {
        let scoring = scoring.ok_or_else(|| {
            SQLError::Internal(
                "OperatorTree::Term reached EngineDriver without bound text scoring".into(),
            )
        })?;
        let field_expr = match field {
            Some(f) => ScalarExpr::Column(f.to_string()),
            None => ScalarExpr::Literal(Value::Str(String::new())),
        };
        let args = vec![
            field_expr,
            ScalarExpr::Literal(Value::Str(query.to_string())),
        ];
        let result = match scoring {
            TextScoringMode::BM25 => {
                sql::run_text_match_public(self.engine, self.table, &args, self.params)
            }
            TextScoringMode::BayesianBM25 => {
                sql::run_bayesian_match_public(self.engine, self.table, &args, self.params)
            }
        };
        result.map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_knn(
        &self,
        query_vector: &[f32],
        k: usize,
        field: &str,
    ) -> DriverResult<PostingList> {
        self.require_vector_query(field, query_vector)?;
        let v_expr = ScalarExpr::Array(
            query_vector
                .iter()
                .map(|x| ScalarExpr::Literal(Value::Float(f64::from(*x))))
                .collect(),
        );
        let args = vec![
            ScalarExpr::Column(field.to_string()),
            v_expr,
            ScalarExpr::Literal(Value::Int(k as i64)),
        ];
        sql::run_knn_match_public(self.engine, self.table, &args, self.params)
            .map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_filter(
        &self,
        field: &str,
        predicate: &Predicate,
        source: Option<&OperatorTree>,
    ) -> DriverResult<PostingList> {
        if !self.engine.has_table(self.table) {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        }
        if !self.engine.table_has_column(self.table, field) {
            return Err(SQLError::UnknownColumn(field.to_string()));
        }
        // Indexed columns resolve through the value index in
        // O(log n + k); the index refuses predicates it cannot answer
        // with evaluated-scan semantics, so this never changes results.
        if let Some(indexed) = self.engine.value_index_scan(self.table, field, predicate) {
            return match source {
                Some(child) => self
                    .execute_posting_node(child)
                    .map(|posting| posting.intersect_owned(&indexed)),
                None => Ok(indexed),
            };
        }
        let candidates: Vec<DocId> = match source {
            Some(child) => {
                let inner = self.execute_posting_node(child)?;
                inner.entries().iter().map(|e| e.doc_id).collect()
            }
            None => self.engine.table_doc_ids(self.table),
        };
        let values = self
            .engine
            .get_document_fields(self.table, &candidates, field);
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(candidates.len());
        for doc_id in candidates {
            if predicate.evaluate(values.get(&doc_id)) {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}

impl OperatorTreeDriver for EngineDriver<'_> {
    type Error = SQLError;

    // Keep one exhaustive physical-dispatch match: adding an IR variant must
    // fail compilation here instead of falling through a category wildcard.
    #[allow(clippy::match_same_arms)]
    #[allow(clippy::too_many_lines)]
    fn execute_node(&self, op: &OperatorTree) -> DriverResult<OperatorOutput> {
        let posting = match op {
            OperatorTree::Empty => Ok(PostingList::new()),
            OperatorTree::Term {
                query,
                field,
                scoring,
            } => self.execute_term(query, field.as_deref(), *scoring),
            OperatorTree::BayesianScore { source, field } => {
                self.execute_bayesian_score(source, field.as_deref())
            }
            OperatorTree::KNN {
                query_vector,
                k,
                field,
            } => self.execute_knn(query_vector, *k, field),
            OperatorTree::Filter {
                field,
                predicate,
                source,
            } => self.execute_filter(field, predicate, source.as_deref()),
            OperatorTree::Facet { field, source } => self.execute_facet(field, source.as_deref()),
            OperatorTree::Score {
                scorer,
                source,
                query_terms,
                field,
            } => self.execute_score(scorer, source, query_terms, field),
            OperatorTree::Intersect(parts) => return self.execute_intersect(parts),
            OperatorTree::Union(parts) => return self.execute_union(parts),
            OperatorTree::Complement(inner) => self.execute_complement(inner),
            OperatorTree::Composed(parts) => return self.execute_composed(parts),
            OperatorTree::VectorSimilarity {
                query_vector,
                threshold,
                field,
            } => self.execute_vector_similarity(query_vector, *threshold, field),
            OperatorTree::LogOddsFusion {
                signals,
                alpha,
                gating,
                weights,
                logit_min,
                logit_max,
            } => self.execute_log_odds_fusion(
                signals,
                *alpha,
                gating,
                weights.as_deref(),
                logit_min.as_deref(),
                logit_max.as_deref(),
            ),
            OperatorTree::ProbBoolFusion { signals, mode } => {
                self.execute_prob_bool_fusion(signals, *mode)
            }
            OperatorTree::ProbNot {
                signal,
                default_prob,
            } => self.execute_prob_not(signal, *default_prob),
            OperatorTree::IndexScan {
                index_name,
                field,
                predicate,
            } => self.execute_index_scan(index_name, field, predicate),
            OperatorTree::Aggregate {
                source,
                field,
                monoid,
            } => self.execute_aggregate(source.as_deref(), field, monoid),
            OperatorTree::GroupBy {
                source,
                group_field,
                agg_field,
                monoid,
            } => self.execute_group_by(source, group_field, agg_field, monoid),
            OperatorTree::HybridTextVector {
                term_op,
                vector_op,
                alpha,
            } => self.execute_hybrid_text_vector(term_op, vector_op, *alpha),
            OperatorTree::SemanticFilter { source, vector_op } => {
                self.execute_semantic_filter(source, vector_op)
            }
            OperatorTree::VectorExclusion { positive, negative } => {
                self.execute_vector_exclusion(positive, negative)
            }
            OperatorTree::FacetVector {
                vector_op,
                facet_field,
            } => self.execute_facet_vector(vector_op, facet_field),
            OperatorTree::CosineProbability(source) => self.execute_cosine_probability(source),
            OperatorTree::AttentionFusion {
                signals,
                attention,
                query_features,
            } => self.execute_attention_fusion(signals, attention, query_features),
            OperatorTree::LearnedFusion { signals, learned } => {
                self.execute_learned_fusion(signals, learned)
            }
            OperatorTree::SparseThreshold { source, threshold } => {
                let source = self.execute_posting_node(source)?;
                Ok(sparse_threshold_inline(&source, *threshold))
            }
            OperatorTree::MultiFieldSearch {
                fields,
                query,
                weights,
            } => self.execute_multi_field_search(fields, query, weights.as_deref()),
            OperatorTree::MultiStage { stages } => self.execute_multi_stage(stages),
            OperatorTree::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
                vertex_predicate,
            } => self.execute_traverse(
                *start_vertex,
                graph,
                label.as_deref(),
                *max_hops,
                vertex_predicate.as_ref(),
            ),
            OperatorTree::PatternMatch { pattern, graph } => {
                self.execute_pattern_match(pattern, graph)
            }
            OperatorTree::RegularPathQuery {
                rpq_source,
                start_vertex,
                graph,
            } => self.execute_regular_path_query(rpq_source, *start_vertex, graph),
            OperatorTree::GraphJoin {
                left,
                right,
                label,
                graph,
            } => {
                return self
                    .execute_graph_join(left, right, label.as_deref(), graph)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::VertexAggregation { source, monoid } => {
                self.execute_vertex_aggregation(source, monoid)
            }
            OperatorTree::WeightedPathQuery {
                rpq_source,
                start_vertex,
                graph,
                weight_property,
                default_edge_weight,
                max_hops,
                predicate,
                predicate_selectivity,
                score,
            } => self.execute_weighted_path_query(WeightedPathExecution {
                rpq_source,
                start_vertex: *start_vertex,
                graph,
                weight_property,
                default_edge_weight: *default_edge_weight,
                max_hops: *max_hops,
                predicate,
                predicate_selectivity: *predicate_selectivity,
                score: *score,
            }),
            OperatorTree::MessagePassing { source } => self.execute_message_passing(source),
            OperatorTree::GraphEmbedding { source } => self.execute_graph_embedding(source),
            OperatorTree::PageRank { graph } => self.execute_page_rank(graph),
            OperatorTree::HITS { graph } => self.execute_hits(graph),
            OperatorTree::BetweennessCentrality { graph } => {
                self.execute_betweenness_centrality(graph)
            }
            OperatorTree::TextSimilarityJoin {
                left,
                right,
                threshold,
            } => {
                return self
                    .execute_text_similarity_join(left, right, *threshold)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::VectorSimilarityJoin {
                left,
                right,
                threshold,
            } => {
                return self
                    .execute_vector_similarity_join(left, right, *threshold)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::HybridJoin { left, right } => {
                return self
                    .execute_hybrid_join(left, right)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::CrossParadigmJoin { left, right } => {
                return self
                    .execute_cross_paradigm_join(left, right)
                    .map(OperatorOutput::Generalized);
            }
            OperatorTree::TemporalTraverse {
                start_vertex,
                graph,
                label,
                max_hops,
                temporal_filter,
            } => self.execute_temporal_traverse(
                *start_vertex,
                graph,
                label.as_deref(),
                *max_hops,
                temporal_filter.as_ref(),
            ),
            OperatorTree::TemporalPatternMatch {
                pattern,
                graph,
                temporal_filter,
            } => self.execute_temporal_pattern_match(pattern, graph, temporal_filter.as_ref()),
            OperatorTree::ProgressiveFusion {
                stages,
                alpha,
                gating,
            } => self.execute_progressive_fusion(stages, *alpha, gating),
            OperatorTree::DeepFusion {
                layers,
                alpha,
                gating,
            } => self.execute_deep_fusion(layers, *alpha, gating),
            OperatorTree::Opaque {
                kind,
                children,
                meta,
            } => self.execute_opaque(kind, children, meta),
        }?;
        Ok(OperatorOutput::Posting(posting))
    }
}

impl EngineDriver<'_> {
    fn execute_intersect(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let mut iter = self.execute_output_branches(parts)?.into_iter();
        let Some(first) = iter.next() else {
            return Ok(PostingList::new().into());
        };
        iter.try_fold(first, |acc, next| match (acc, next) {
            (OperatorOutput::Posting(left), OperatorOutput::Posting(right)) => {
                Ok(OperatorOutput::Posting(left.intersect_owned(&right)))
            }
            (OperatorOutput::Generalized(left), OperatorOutput::Generalized(right)) => {
                Ok(OperatorOutput::Generalized(left.intersect(&right)))
            }
            _ => Err(SQLError::TypeMismatch(
                "Intersect operands must use the same posting-list carrier".to_string(),
            )),
        })
    }

    fn execute_union(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let mut iter = self.execute_output_branches(parts)?.into_iter();
        let Some(first) = iter.next() else {
            return Ok(PostingList::new().into());
        };
        iter.try_fold(first, |acc, next| match (acc, next) {
            (OperatorOutput::Posting(left), OperatorOutput::Posting(right)) => {
                Ok(OperatorOutput::Posting(left.union(&right)))
            }
            (OperatorOutput::Generalized(left), OperatorOutput::Generalized(right)) => {
                Ok(OperatorOutput::Generalized(left.union(&right)))
            }
            _ => Err(SQLError::TypeMismatch(
                "Union operands must use the same posting-list carrier".to_string(),
            )),
        })
    }

    fn execute_complement(&self, inner: &OperatorTree) -> DriverResult<PostingList> {
        if !self.engine.has_table(self.table) {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        }
        let inner_pl = self.execute_posting_node(inner)?;
        let included: BTreeSet<DocId> = inner_pl.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for doc_id in self.engine.table_doc_ids(self.table) {
            if !included.contains(&doc_id) {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn execute_composed(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let mut result = OperatorOutput::Posting(PostingList::new());
        for part in parts {
            result = self.execute_node(part)?;
        }
        Ok(result)
    }

    fn execute_facet(
        &self,
        field: &str,
        source: Option<&OperatorTree>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{FacetOperator, Operator};

        self.require_column(field)?;
        let source = source
            .map(|child| self.execute_posting_node(child).map(static_operator))
            .transpose()?;
        let op = FacetOperator::new(field, source);
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_score(
        &self,
        scorer: &uqa_operators::ScorerRef,
        source: &OperatorTree,
        query_terms: &[String],
        field: &str,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, ScoreOperator};

        self.require_column(field)?;
        let source = static_operator(self.execute_posting_node(source)?);
        let op = ScoreOperator::new(scorer.clone(), source, query_terms.to_vec(), field);
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_vector_similarity(
        &self,
        query_vector: &[f32],
        threshold: f32,
        field: &str,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, VectorSimilarityOperator};

        self.require_vector_query(field, query_vector)?;
        if !threshold.is_finite() || !(-1.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "VectorSimilarity.threshold must be finite and in [-1, 1], got {threshold}"
            )));
        }
        let op = VectorSimilarityOperator::new(query_vector.to_vec(), threshold, field);
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_aggregate(
        &self,
        source: Option<&OperatorTree>,
        field: &str,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{AggregateOperator, Operator};

        self.require_column(field)?;
        let source = source
            .map(|child| self.execute_posting_node(child).map(static_operator))
            .transpose()?;
        let op = AggregateOperator::new(source, field, monoid.clone());
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_group_by(
        &self,
        source: &OperatorTree,
        group_field: &str,
        agg_field: &str,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{GroupByOperator, Operator};

        self.require_column(group_field)?;
        self.require_column(agg_field)?;
        let source = static_operator(self.execute_posting_node(source)?);
        let op = GroupByOperator::new(source, group_field, agg_field, monoid.clone());
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_hybrid_text_vector(
        &self,
        term_op: &OperatorTree,
        vector_op: &OperatorTree,
        alpha: f64,
    ) -> DriverResult<PostingList> {
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "HybridTextVector.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }
        let text = self.execute_posting_node(term_op)?;
        let vector = self.execute_posting_node(vector_op)?;
        let text_scores: BTreeMap<DocId, f64> = text
            .entries()
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect();
        let vector_scores: BTreeMap<DocId, f64> = vector
            .entries()
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect();
        let intersection = text.intersect_owned(&vector);
        Ok(intersection.with_scores(|entry| {
            let text_score = text_scores.get(&entry.doc_id).copied().unwrap_or_default();
            let vector_score = vector_scores
                .get(&entry.doc_id)
                .copied()
                .unwrap_or_default();
            alpha * text_score + (1.0 - alpha) * vector_score
        }))
    }

    fn execute_semantic_filter(
        &self,
        source: &OperatorTree,
        vector_op: &OperatorTree,
    ) -> DriverResult<PostingList> {
        let source = self.execute_posting_node(source)?;
        let vector = self.execute_posting_node(vector_op)?;
        Ok(source.intersect_owned(&vector))
    }

    fn execute_traverse(
        &self,
        start_vertex: u64,
        graph: &str,
        label: Option<&str>,
        max_hops: usize,
        vertex_predicate: Option<&uqa_operators::VertexPredicate>,
    ) -> DriverResult<PostingList> {
        let max_hops = u32::try_from(max_hops).map_err(|_| {
            SQLError::TypeMismatch(format!("Traverse.max_hops is too large: {max_hops}"))
        })?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::Traverse::new(start_vertex, graph).max_hops(max_hops);
            if let Some(label) = label {
                op = op.label(label);
            }
            if let Some(predicate) = vertex_predicate {
                op = op.predicate(uqa_graph::VertexPredicate::Custom(predicate.clone()));
            }
            op.execute(store).to_posting_list()
        })
    }

    fn execute_pattern_match(
        &self,
        pattern: &uqa_operators::GraphPatternIR,
        graph: &str,
    ) -> DriverResult<PostingList> {
        let pattern = graph_pattern_from_ir(pattern);
        self.with_graph(graph, |store| {
            uqa_graph::GMatch::new(pattern, graph)
                .execute(store)
                .to_posting_list()
        })
    }

    fn execute_regular_path_query(
        &self,
        rpq_source: &str,
        start_vertex: u64,
        graph: &str,
    ) -> DriverResult<PostingList> {
        let path = parse_rpq(rpq_source)?;
        self.with_graph(graph, |store| {
            uqa_graph::RegularPathQuery::new(path, graph)
                .from_vertex(start_vertex)
                .execute(store)
                .to_posting_list()
        })
    }

    fn execute_graph_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        label: Option<&str>,
        graph: &str,
    ) -> DriverResult<GeneralizedPostingList> {
        let left = self.execute_posting_node(left)?;
        let right = self.execute_posting_node(right)?;
        self.with_graph(graph, |store| {
            let mut op = uqa_joins::GraphJoin::new(left.entries(), right.entries(), store, graph);
            if let Some(label) = label {
                op = op.label(label);
            }
            op.execute()
        })
    }

    fn execute_vertex_aggregation(
        &self,
        source: &OperatorTree,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        let source = self.execute_posting_node(source)?;
        let mut state = monoid.identity();
        for entry in source.entries() {
            state = monoid.accumulate(state, &Value::Float(entry.payload.score));
        }
        let result = monoid.finalize(state);
        let score = numeric_score(&result);
        let mut fields = BTreeMap::new();
        fields.insert("_vertex_aggregate".to_string(), result);
        fields.insert(
            "_vertex_aggregate_count".to_string(),
            Value::Int(source.len() as i64),
        );
        Ok(PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            0,
            Payload {
                score,
                fields,
                ..Default::default()
            },
        )]))
    }

    fn execute_weighted_path_query(
        &self,
        query: WeightedPathExecution<'_>,
    ) -> DriverResult<PostingList> {
        let WeightedPathExecution {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        } = query;
        if !predicate_selectivity.is_finite() || !(0.0..=1.0).contains(&predicate_selectivity) {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.predicate_selectivity must be finite and in [0, 1], got {predicate_selectivity}"
            )));
        }
        if weight_property.is_empty() {
            return Err(SQLError::TypeMismatch(
                "WeightedPathQuery.weight_property must not be empty".to_string(),
            ));
        }
        if !default_edge_weight.is_finite() {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.default_edge_weight must be finite, got {default_edge_weight}"
            )));
        }
        if !score.is_finite() {
            return Err(SQLError::TypeMismatch(format!(
                "WeightedPathQuery.score must be finite, got {score}"
            )));
        }
        let path = parse_rpq(rpq_source)?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::WeightedPathQuery::new(
                path,
                graph,
                weight_property,
                std::sync::Arc::clone(predicate),
            )
            .from_vertex(start_vertex);
            op.default_edge_weight = default_edge_weight;
            op.max_hops = max_hops;
            op.score = score;
            op.execute(store).to_posting_list()
        })
    }

    fn execute_message_passing(&self, source: &OperatorTree) -> DriverResult<PostingList> {
        let graph = require_graph_name(source, "MessagePassing.source")?;
        let source_result = self.execute_posting_node(source)?;
        let result = self.with_graph(&graph, |store| {
            uqa_graph::MessagePassing::new(&graph)
                .execute(store)
                .to_posting_list()
        })?;
        Ok(restrict_result_to_source(&result, &source_result))
    }

    fn execute_graph_embedding(&self, source: &OperatorTree) -> DriverResult<PostingList> {
        let graph = require_graph_name(source, "GraphEmbedding.source")?;
        let source_result = self.execute_posting_node(source)?;
        let result = self.with_graph(&graph, |store| {
            uqa_graph::GraphEmbedding::new(&graph)
                .execute(store)
                .to_posting_list()
        })?;
        Ok(restrict_result_to_source(&result, &source_result))
    }

    fn execute_page_rank(&self, graph: &str) -> DriverResult<PostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::PageRank::new(graph)
                .execute(store)
                .to_posting_list()
        })
    }

    fn execute_hits(&self, graph: &str) -> DriverResult<PostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::HITS::new(graph).execute(store).to_posting_list()
        })
    }

    fn execute_betweenness_centrality(&self, graph: &str) -> DriverResult<PostingList> {
        self.with_graph(graph, |store| {
            uqa_graph::BetweennessCentrality::new(graph)
                .execute(store)
                .to_posting_list()
        })
    }

    fn execute_text_similarity_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "TextSimilarityJoin.threshold must be finite and in [0, 1], got {threshold}"
            )));
        }
        let left_field = require_text_field(left, "TextSimilarityJoin.left")?;
        let right_field = require_text_field(right, "TextSimilarityJoin.right")?;
        let left_source = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let left = self.prepare_join_operand(&left_source, &left_field, "_join_text")?;
        let right = self.prepare_join_operand(&right_source, &right_field, "_join_text")?;
        Ok(uqa_joins::TextSimilarityJoin::new(
            left.entries(),
            right.entries(),
            "_join_text",
            "_join_text",
        )
        .threshold(threshold)
        .execute())
    }

    fn execute_vector_similarity_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        if !threshold.is_finite() || !(-1.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "VectorSimilarityJoin.threshold must be finite and in [-1, 1], got {threshold}"
            )));
        }
        let left_field = require_vector_field(left, "VectorSimilarityJoin.left")?;
        let right_field = require_vector_field(right, "VectorSimilarityJoin.right")?;
        let left_source = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let left = self.prepare_join_operand(&left_source, &left_field, "_join_vector")?;
        let right = self.prepare_join_operand(&right_source, &right_field, "_join_vector")?;
        Ok(uqa_joins::VectorSimilarityJoin::new(
            left.entries(),
            right.entries(),
            "_join_vector",
            "_join_vector",
        )
        .threshold(threshold)
        .execute())
    }

    fn execute_hybrid_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let structured_field = require_shared_structured_field(left, right, "HybridJoin")?;
        let vector_field = require_shared_vector_field(left, right, "HybridJoin")?;
        let left_result = self.execute_posting_node(left)?;
        let right_result = self.execute_posting_node(right)?;
        let left_keyed =
            self.prepare_join_operand(&left_result, &structured_field.0, "_join_key")?;
        let left_result =
            self.prepare_join_operand(&left_keyed, &vector_field.0, "_join_vector")?;
        let right_keyed =
            self.prepare_join_operand(&right_result, &structured_field.1, "_join_key")?;
        let right_result =
            self.prepare_join_operand(&right_keyed, &vector_field.1, "_join_vector")?;
        Ok(uqa_joins::HybridJoin::new(
            left_result.entries(),
            right_result.entries(),
            "_join_key",
            "_join_vector",
        )
        .execute())
    }

    fn execute_cross_paradigm_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let graph = require_graph_name(left, "CrossParadigmJoin.left")?;
        let vertex_field = first_structured_field(left)
            .or_else(|| first_structured_field(right))
            .ok_or_else(|| {
                SQLError::TypeMismatch(
                    "CrossParadigmJoin operands do not identify a join property".to_string(),
                )
            })?;
        let doc_field = first_structured_field(right).unwrap_or_else(|| vertex_field.clone());
        let left_result = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let right_result =
            self.prepare_join_operand(&right_source, &doc_field, "_join_document")?;
        self.with_graph(&graph, |store| {
            uqa_joins::CrossParadigmJoin::new(
                left_result.entries(),
                right_result.entries(),
                store,
                &vertex_field,
                "_join_document",
            )
            .execute()
        })
    }

    fn prepare_join_operand(
        &self,
        source: &PostingList,
        field: &str,
        alias: &str,
    ) -> DriverResult<PostingList> {
        let document_store = self
            .engine
            .table(self.table)
            .map(|table| table.document_store.read().snapshot());
        let requires_document_lookup = source
            .entries()
            .iter()
            .any(|entry| !entry.payload.fields.contains_key(field));
        if document_store.is_some() && requires_document_lookup {
            self.require_column(field)?;
        }
        let entries = source
            .entries()
            .iter()
            .map(|entry| {
                let mut payload = entry.payload.clone();
                let value = payload.fields.get(field).cloned().or_else(|| {
                    document_store
                        .as_ref()
                        .and_then(|store| store.get_field(entry.doc_id, field))
                });
                if let Some(value) = value {
                    payload.fields.insert(alias.to_string(), value);
                }
                PostingEntry::new(entry.doc_id, payload)
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn execute_temporal_traverse(
        &self,
        start_vertex: u64,
        graph: &str,
        label: Option<&str>,
        max_hops: usize,
        temporal_filter: Option<&uqa_operators::TemporalFilterIR>,
    ) -> DriverResult<PostingList> {
        let max_hops = u32::try_from(max_hops).map_err(|_| {
            SQLError::TypeMismatch(format!(
                "TemporalTraverse.max_hops is too large: {max_hops}"
            ))
        })?;
        let filter = temporal_filter_from_ir(temporal_filter)?;
        self.with_graph(graph, |store| {
            let mut op = uqa_graph::TemporalTraverse::new(start_vertex, graph)
                .max_hops(max_hops)
                .filter(filter);
            if let Some(label) = label {
                op = op.label(label);
            }
            op.execute(store).to_posting_list()
        })
    }

    fn execute_temporal_pattern_match(
        &self,
        pattern: &uqa_operators::GraphPatternIR,
        graph: &str,
        temporal_filter: Option<&uqa_operators::TemporalFilterIR>,
    ) -> DriverResult<PostingList> {
        let pattern = graph_pattern_from_ir(pattern);
        let filter = temporal_filter_from_ir(temporal_filter)?;
        self.with_graph(graph, |store| {
            uqa_graph::TemporalPatternMatch::new(pattern, graph)
                .filter(filter)
                .execute(store)
                .to_posting_list()
        })
    }

    fn with_graph<R>(
        &self,
        graph: &str,
        execute: impl FnOnce(&uqa_graph::MemoryGraphStore) -> R,
    ) -> DriverResult<R> {
        self.engine
            .graph_with(graph, execute)
            .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))
    }

    fn execute_facet_vector(
        &self,
        vector_op: &OperatorTree,
        facet_field: &str,
    ) -> DriverResult<PostingList> {
        let vec_pl = self.execute_posting_node(vector_op)?;
        self.facet_vector_inline(&vec_pl, facet_field)
    }

    fn execute_prob_bool_fusion(
        &self,
        signals: &[OperatorTree],
        mode: uqa_operators::ProbBoolMode,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;
        use uqa_operators::{HybridProbBoolMode, ProbBoolFusionOperator};
        if signals.is_empty() {
            return Ok(PostingList::new());
        }
        // Pre-execute every child through the driver, then wrap the
        // results in static signal operators so the fusion operator can
        // consume them without taking a back-reference into the driver.
        let signal_ops: Vec<std::sync::Arc<dyn Operator>> = self
            .execute_posting_branches(signals)?
            .into_iter()
            .map(|pl| -> std::sync::Arc<dyn Operator> {
                std::sync::Arc::new(StaticPostingList { pl })
            })
            .collect();
        let mode = match mode {
            uqa_operators::ProbBoolMode::And => HybridProbBoolMode::And,
            uqa_operators::ProbBoolMode::Or => HybridProbBoolMode::Or,
        };
        let op = ProbBoolFusionOperator::new(signal_ops, mode);
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_multi_field_search(
        &self,
        fields: &[String],
        query: &str,
        weights: Option<&[f64]>,
    ) -> DriverResult<PostingList> {
        // Delegate to the row-function implementation like the other
        // leaf nodes, so every lowering of `multi_field_match` shares
        // one pad, one per-field analyzer choice, and one stats source.
        let mut args: Vec<ScalarExpr> = fields
            .iter()
            .map(|field| ScalarExpr::Column(field.clone()))
            .collect();
        args.push(ScalarExpr::Literal(Value::Str(query.to_string())));
        if let Some(weights) = weights {
            args.extend(
                weights
                    .iter()
                    .map(|weight| ScalarExpr::Literal(Value::Float(*weight))),
            );
        }
        sql::run_multi_field_match_public(self.engine, self.table, &args, self.params)
            .map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_bayesian_match_with_prior(
        &self,
        meta: &BTreeMap<String, Value>,
    ) -> DriverResult<PostingList> {
        let Some(Value::Str(field)) = meta.get("field") else {
            return Err(malformed_operator_meta(
                "bayesian_match_with_prior",
                "field",
            ));
        };
        let Some(Value::Str(query)) = meta.get("query") else {
            return Err(malformed_operator_meta(
                "bayesian_match_with_prior",
                "query",
            ));
        };
        let Some(Value::Str(prior_field)) = meta.get("prior_field") else {
            return Err(malformed_operator_meta(
                "bayesian_match_with_prior",
                "prior_field",
            ));
        };
        let Some(Value::Str(mode)) = meta.get("mode") else {
            return Err(malformed_operator_meta("bayesian_match_with_prior", "mode"));
        };
        let args = vec![
            ScalarExpr::Column(field.clone()),
            ScalarExpr::Literal(Value::Str(query.clone())),
            ScalarExpr::Column(prior_field.clone()),
            ScalarExpr::Literal(Value::Str(mode.clone())),
        ];
        sql::run_bayesian_match_with_prior_public(self.engine, self.table, &args, self.params)
            .map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_calibrated_vector_match(
        &self,
        meta: &BTreeMap<String, Value>,
    ) -> DriverResult<PostingList> {
        let Some(Value::Str(field)) = meta.get("field") else {
            return Err(malformed_operator_meta("calibrated_vector_match", "field"));
        };
        let Some(Value::List(query_vector)) = meta.get("query_vector") else {
            return Err(malformed_operator_meta(
                "calibrated_vector_match",
                "query_vector",
            ));
        };
        let Some(Value::Int(k)) = meta.get("k") else {
            return Err(malformed_operator_meta("calibrated_vector_match", "k"));
        };
        let physical_query = value_to_vector(&Value::List(query_vector.clone()))?;
        self.require_vector_query(field, &physical_query)?;
        let mut args = vec![
            ScalarExpr::Literal(Value::Str(field.clone())),
            ScalarExpr::Array(
                query_vector
                    .iter()
                    .cloned()
                    .map(ScalarExpr::Literal)
                    .collect(),
            ),
            ScalarExpr::Literal(Value::Int(*k)),
        ];
        if let Some(threshold) = meta.get("threshold") {
            args.push(ScalarExpr::Literal(threshold.clone()));
        }
        sql::run_calibrated_vector_match_public(self.engine, self.table, &args, self.params)
            .map(|rows| scored_to_posting_list(&rows))
    }

    fn execute_prob_not(
        &self,
        signal: &OperatorTree,
        default_prob: f64,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;
        use uqa_operators::ProbNotOperator;
        let signal_pl = self.execute_posting_node(signal)?;
        let signal_op: std::sync::Arc<dyn Operator> =
            std::sync::Arc::new(StaticPostingList { pl: signal_pl });
        let op = ProbNotOperator::new(signal_op, default_prob);
        Ok(op.execute(&self.bridge_context()?))
    }

    fn execute_index_scan(
        &self,
        index_name: &str,
        field: &str,
        predicate: &uqa_core::Predicate,
    ) -> DriverResult<PostingList> {
        self.require_column(field)?;
        let index = self.engine.catalog_index(index_name).ok_or_else(|| {
            SQLError::Unsupported(format!("unknown physical index {index_name:?}"))
        })?;
        let resolved_table = self
            .engine
            .resolve_table_name(self.table)
            .unwrap_or_else(|| self.table.to_string());
        if index.table_name != resolved_table {
            return Err(SQLError::TypeMismatch(format!(
                "index {index_name:?} belongs to table {:?}, not {:?}",
                index.table_name, self.table
            )));
        }
        if !index.index_type.eq_ignore_ascii_case("btree") {
            return Err(SQLError::TypeMismatch(format!(
                "IndexScan requires a btree index, but {index_name:?} is {:?}",
                index.index_type
            )));
        }
        let columns: Vec<String> = serde_json::from_str(&index.columns_json).map_err(|error| {
            SQLError::Internal(format!(
                "index {index_name:?} has malformed column metadata: {error}"
            ))
        })?;
        if columns.first().is_none_or(|column| column != field) {
            return Err(SQLError::TypeMismatch(format!(
                "index {index_name:?} does not cover leading field {field:?}"
            )));
        }
        self.engine
            .value_index_scan(self.table, field, predicate)
            .ok_or_else(|| {
                SQLError::Unsupported(format!(
                    "index {index_name:?} cannot evaluate predicate {predicate:?}"
                ))
            })
    }

    fn execute_vector_exclusion(
        &self,
        positive: &OperatorTree,
        negative: &OperatorTree,
    ) -> DriverResult<PostingList> {
        let pos = self.execute_posting_node(positive)?;
        let neg = self.execute_posting_node(negative)?;
        let neg_ids: BTreeSet<DocId> = neg.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in pos.entries() {
            if !neg_ids.contains(&entry.doc_id) {
                entries.push(entry.clone());
            }
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn execute_log_odds_fusion(
        &self,
        signals: &[OperatorTree],
        alpha: f64,
        gating: &GatingSpec,
        weights: Option<&[f64]>,
        logit_min: Option<&[f64]>,
        logit_max: Option<&[f64]>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;

        if signals.is_empty() {
            return Ok(PostingList::new());
        }
        let mut signal_ops: Vec<std::sync::Arc<dyn Operator>> = Vec::with_capacity(signals.len());
        let mut signal_priors: Vec<f64> = Vec::new();
        for signal in signals {
            let (pl, prior) = self.execute_fusion_signal(signal)?;
            signal_ops.push(std::sync::Arc::new(StaticPostingList { pl }));
            if let Some(prior) = prior {
                signal_priors.push(prior);
            }
        }
        let logit_gating = match gating {
            GatingSpec::Softplus => uqa_fusion::LogitGating::Softplus,
            GatingSpec::Pass => uqa_fusion::LogitGating::Pass,
            GatingSpec::Sigmoid { .. } => uqa_fusion::LogitGating::Sigmoid,
            GatingSpec::ReLU => uqa_fusion::LogitGating::ReLU,
            GatingSpec::Swish => uqa_fusion::LogitGating::Swish,
            GatingSpec::Gelu => uqa_fusion::LogitGating::Gelu,
        };
        let mut operator = LogOddsFusionOperator::new(signal_ops, alpha).with_gating(logit_gating);
        if let Some(base_rate) = combine_signal_priors(&signal_priors) {
            operator = operator.with_base_rate(base_rate);
        }
        if let Some(weights) = weights {
            operator = operator.with_weights(weights.to_vec());
        }
        if let (Some(logit_min), Some(logit_max)) = (logit_min, logit_max) {
            operator = operator.with_logit_normalization(logit_min.to_vec(), logit_max.to_vec());
        }
        Ok(operator.execute(&self.bridge_context()?))
    }

    /// Execute a fusion child under the probability contract: the
    /// signal contributes prior-free evidence and reports the corpus
    /// relevance prior it would otherwise have folded in, so the
    /// fusion can apply that prior exactly once.
    fn execute_fusion_signal(
        &self,
        signal: &OperatorTree,
    ) -> DriverResult<(PostingList, Option<f64>)> {
        match signal {
            OperatorTree::BayesianScore { source, field } => {
                let params = field
                    .as_deref()
                    .map_or_else(uqa_scoring::BayesianBM25Params::default, |field| {
                        self.engine.bayesian_params_for(self.table, field)
                    })
                    .scaled_for_query_terms(scored_term_count(source));
                let prior = (params.base_rate > 0.0).then_some(params.base_rate);
                let evidence_params = params.evidence_params();
                let raw = self.execute_posting_node(source)?;
                let evidence = raw.with_scores(|entry| {
                    uqa_scoring::sigmoid(
                        evidence_params.alpha * (entry.payload.score - evidence_params.beta),
                    )
                });
                Ok((evidence, prior))
            }
            OperatorTree::Term {
                query,
                field,
                scoring: Some(TextScoringMode::BayesianBM25),
            } => {
                let field_expr = match field {
                    Some(f) => ScalarExpr::Column(f.clone()),
                    None => ScalarExpr::Literal(Value::Str(String::new())),
                };
                let args = vec![field_expr, ScalarExpr::Literal(Value::Str(query.clone()))];
                let rows = sql::run_bayesian_evidence_match_public(
                    self.engine,
                    self.table,
                    &args,
                    self.params,
                )?;
                Ok((
                    scored_to_posting_list(&rows),
                    self.text_field_prior(field.as_deref()),
                ))
            }
            OperatorTree::CosineProbability(source) => self
                .execute_cosine_evidence(source)
                .map(|posting| (posting, None)),
            other => self
                .execute_posting_node(other)
                .map(|posting| (posting, None)),
        }
    }

    /// The corpus relevance prior of a text field, or the logit-mean
    /// prior across every text-indexed field for `_all` queries.
    fn text_field_prior(&self, field: Option<&str>) -> Option<f64> {
        let priors: Vec<f64> = match field {
            Some(field) => vec![self.engine.bayesian_params_for(self.table, field).base_rate],
            None => self
                .engine
                .fts_fields_for_table(self.table)
                .iter()
                .map(|field| self.engine.bayesian_params_for(self.table, field).base_rate)
                .collect(),
        };
        combine_signal_priors(
            &priors
                .into_iter()
                .filter(|rate| *rate > 0.0)
                .collect::<Vec<_>>(),
        )
    }

    /// Likelihood-ratio calibrated vector evidence: fit the pool
    /// calibration on the source's cosine similarities and emit
    /// prior-free posteriors (base rate 0.5 contributes zero log-odds).
    fn execute_cosine_evidence(&self, source: &OperatorTree) -> DriverResult<PostingList> {
        let pl = self.execute_posting_node(source)?;
        let distances: Vec<f64> = pl.iter().map(|e| 1.0 - e.payload.score).collect();
        let calibrated = match uqa_operators::fit_pool_calibration(
            &distances,
            uqa_operators::RelevantSampleSplit::default(),
            0.5,
        ) {
            Some(transform) => pl.with_scores(|e| {
                transform
                    .calibrate_one(1.0 - e.payload.score)
                    .clamp(1e-6, 1.0 - 1e-6)
            }),
            None => pl.with_scores(|_| 0.5),
        };
        Ok(calibrated)
    }

    fn execute_cosine_probability(&self, source: &OperatorTree) -> DriverResult<PostingList> {
        // Lift cosine similarities in `[-1, 1]` onto the (0, 1)
        // probability scale via `(1 + s) / 2`. Mirrors
        // [`uqa_operators::CosineProbabilityOperator`] but skips the
        // trait wrapper because the source has already been driven
        // through the engine. Standalone `knn_match` keeps this
        // Definition 7.1.2 map; fusion contexts route through
        // [`Self::execute_cosine_evidence`] instead.
        use uqa_scoring::cosine_to_probability;
        let pl = self.execute_posting_node(source)?;
        Ok(pl.with_scores(|e| cosine_to_probability(e.payload.score)))
    }

    fn execute_bayesian_score(
        &self,
        source: &OperatorTree,
        field: Option<&str>,
    ) -> DriverResult<PostingList> {
        let raw = self.execute_posting_node(source)?;
        let params = field
            .map_or_else(uqa_scoring::BayesianBM25Params::default, |field| {
                self.engine.bayesian_params_for(self.table, field)
            })
            .scaled_for_query_terms(scored_term_count(source));
        Ok(raw.with_scores(|entry| {
            uqa_scoring::sigmoid(params.alpha * (entry.payload.score - params.beta))
        }))
    }

    fn execute_attention_fusion(
        &self,
        signals: &[OperatorTree],
        attention: &uqa_operators::tree::AttentionRef,
        query_features: &[f64],
    ) -> DriverResult<PostingList> {
        if signals.is_empty() {
            return Ok(PostingList::new());
        }
        let posting_lists = self.execute_posting_branches(signals)?;
        let features = self.attention_query_features(signals, query_features);
        Ok(fuse_signals_with(&posting_lists, |probs| {
            attention.fuse(probs, &features)
        }))
    }

    fn execute_learned_fusion(
        &self,
        signals: &[OperatorTree],
        learned: &uqa_operators::tree::LearnedFusionRef,
    ) -> DriverResult<PostingList> {
        if signals.is_empty() {
            return Ok(PostingList::new());
        }
        let posting_lists = self.execute_posting_branches(signals)?;
        Ok(fuse_signals_with(&posting_lists, |probs| {
            learned.fuse(probs)
        }))
    }

    fn execute_multi_stage(&self, stages: &[MultiStageEntry]) -> DriverResult<PostingList> {
        let mut current: Option<PostingList> = None;
        for stage in stages {
            let stage_result = self.execute_posting_node(&stage.child)?;
            let mut entries: Vec<PostingEntry> = if let Some(prior) = &current {
                let prior_ids: BTreeSet<DocId> = prior.entries().iter().map(|e| e.doc_id).collect();
                stage_result
                    .entries()
                    .iter()
                    .filter(|entry| prior_ids.contains(&entry.doc_id))
                    .cloned()
                    .collect()
            } else {
                stage_result.entries().to_vec()
            };
            entries.sort_by(|a, b| {
                b.payload
                    .score
                    .partial_cmp(&a.payload.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.doc_id.cmp(&b.doc_id))
            });
            let keep = match stage.cutoff {
                MultiStageCutoff::TopK(k) => k,
                MultiStageCutoff::Ratio(r) => ((entries.len() as f64) * r).ceil() as usize,
            };
            entries.truncate(keep);
            entries.sort_by_key(|e| e.doc_id);
            current = Some(PostingList::from_sorted_unchecked(entries));
        }
        Ok(current.unwrap_or_default())
    }

    fn execute_progressive_fusion(
        &self,
        stages: &[uqa_operators::ProgressiveFusionEntry],
        alpha: f64,
        gating: &GatingSpec,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, ProgressiveFusionOperator};

        if stages.is_empty() {
            return Ok(PostingList::new());
        }
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "ProgressiveFusion.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }
        let mut runtime_stages = Vec::with_capacity(stages.len());
        for stage in stages {
            runtime_stages.push((
                vec![static_operator(self.execute_posting_node(&stage.signal)?)],
                stage.k,
            ));
        }
        let gating = match gating {
            GatingSpec::Softplus => "softplus",
            GatingSpec::Pass => "pass",
            GatingSpec::Sigmoid { .. } => "sigmoid",
            GatingSpec::ReLU => "relu",
            GatingSpec::Swish => "swish",
            GatingSpec::Gelu => "gelu",
        };
        let operator =
            ProgressiveFusionOperator::with_gating(runtime_stages, alpha, Some(gating.into()));
        Ok(operator.execute(&self.bridge_context()?))
    }

    fn execute_deep_fusion(
        &self,
        layers: &[uqa_operators::DeepFusionLayer],
        alpha: f64,
        gating: &GatingSpec,
    ) -> DriverResult<PostingList> {
        use uqa_operators::Operator;

        if layers.is_empty() {
            return Err(SQLError::TypeMismatch(
                "DeepFusion requires at least one layer".to_string(),
            ));
        }
        if !matches!(
            layers.first(),
            Some(uqa_operators::DeepFusionLayer::Signal { .. })
        ) {
            return Err(SQLError::TypeMismatch(
                "DeepFusion's first layer must be Signal".to_string(),
            ));
        }
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "DeepFusion.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }

        let graph_aware = layers.iter().any(|layer| {
            matches!(
                layer,
                uqa_operators::DeepFusionLayer::Propagate { .. }
                    | uqa_operators::DeepFusionLayer::Conv { .. }
                    | uqa_operators::DeepFusionLayer::Pool { .. }
            )
        });
        let mut graph_names = BTreeSet::new();
        for layer in layers {
            if let uqa_operators::DeepFusionLayer::Signal { signals } = layer {
                for signal in signals {
                    collect_graph_names(signal, &mut graph_names);
                }
            }
        }
        if graph_aware && graph_names.len() != 1 {
            return Err(SQLError::TypeMismatch(format!(
                "graph-aware DeepFusion requires exactly one graph-bearing signal, found {graph_names:?}"
            )));
        }

        let runtime_layers = layers
            .iter()
            .map(|layer| self.lower_deep_layer(layer))
            .collect::<DriverResult<Vec<_>>>()?;
        let runtime_gating = deep_runtime_gating(gating);
        let operator = uqa_ml::DeepFusionOperator::new(runtime_layers, alpha, runtime_gating);
        let mut context = self.bridge_context()?;
        if let Some(graph) = graph_names.into_iter().next() {
            let snapshot = self.with_graph(&graph, |store| {
                std::sync::Arc::new(GraphNeighborSnapshot::from_store(store, &graph))
                    as std::sync::Arc<dyn uqa_operators::GraphNeighborLookup>
            })?;
            context.graph = Some(snapshot);
        }
        Ok(operator.execute(&context))
    }

    fn lower_deep_layer(
        &self,
        layer: &uqa_operators::DeepFusionLayer,
    ) -> DriverResult<uqa_ml::Layer> {
        match layer {
            uqa_operators::DeepFusionLayer::Signal { signals } => Ok(uqa_ml::Layer::Signal(
                self.execute_posting_branches(signals)?
                    .into_iter()
                    .map(static_operator)
                    .collect(),
            )),
            uqa_operators::DeepFusionLayer::Propagate {
                edge_label,
                aggregation,
                direction,
            } => Ok(uqa_ml::Layer::Propagate {
                edge_label: edge_label.clone().unwrap_or_default(),
                aggregation: match aggregation {
                    uqa_operators::DeepFusionAggregation::Mean => uqa_ml::DeepAggKind::Mean,
                    uqa_operators::DeepFusionAggregation::Sum => uqa_ml::DeepAggKind::Sum,
                    uqa_operators::DeepFusionAggregation::Max => uqa_ml::DeepAggKind::Max,
                },
                direction: *direction,
            }),
            uqa_operators::DeepFusionLayer::Conv {
                edge_label,
                hop_weights,
                direction,
            } => lower_deep_conv(edge_label.as_deref(), hop_weights, *direction),
            uqa_operators::DeepFusionLayer::Pool {
                edge_label,
                pool_size,
                method,
                direction,
            } => lower_deep_pool(edge_label.as_deref(), *pool_size, *method, *direction),
            uqa_operators::DeepFusionLayer::Flatten => Ok(uqa_ml::Layer::Flatten),
            uqa_operators::DeepFusionLayer::Dense {
                weights,
                bias,
                output_channels,
                input_channels,
            } => lower_deep_dense(weights, bias, *output_channels, *input_channels),
            uqa_operators::DeepFusionLayer::Softmax => Ok(uqa_ml::Layer::Softmax),
            uqa_operators::DeepFusionLayer::BatchNorm { epsilon } => {
                lower_deep_batch_norm(*epsilon)
            }
            uqa_operators::DeepFusionLayer::Dropout { probability } => {
                lower_deep_dropout(*probability)
            }
        }
    }

    fn execute_opaque(
        &self,
        kind: &str,
        children: &[OperatorTree],
        meta: &BTreeMap<String, Value>,
    ) -> DriverResult<PostingList> {
        match kind {
            "bayesian_match_with_prior" if children.is_empty() => {
                self.execute_bayesian_match_with_prior(meta)
            }
            "calibrated_vector_match" if children.is_empty() => {
                self.execute_calibrated_vector_match(meta)
            }
            "deep_predict" if children.is_empty() => {
                let Some(Value::Str(model)) = meta.get("model") else {
                    return Err(malformed_operator_meta("deep_predict", "model"));
                };
                let scores = self
                    .engine
                    .deep_predict(model)
                    .ok_or_else(|| SQLError::Unsupported(format!("unknown model {model:?}")))?;
                Ok(PostingList::from_unsorted(
                    scores
                        .into_iter()
                        .map(|(doc_id, score)| {
                            PostingEntry::new(doc_id, Payload::with_score(score))
                        })
                        .collect(),
                ))
            }
            "bayesian_match_with_prior" | "calibrated_vector_match" | "deep_predict" => Err(
                SQLError::Internal(format!("opaque operator {kind:?} must not have children")),
            ),
            _ => Err(SQLError::UnknownFunction(format!("operator::{kind}"))),
        }
    }

    /// Build the `n_query_features=6` vector that attention fusers
    /// expect. When the IR carries a non-empty explicit vector it wins
    /// (test fixtures); otherwise the driver extracts the canonical
    /// `[mean_idf, max_idf, min_idf, coverage, query_length,
    /// vocab_overlap]` vector from the table's inverted-index stats
    /// against the first text-bearing signal it can find.
    fn attention_query_features(&self, signals: &[OperatorTree], explicit: &[f64]) -> Vec<f64> {
        if !explicit.is_empty() {
            return explicit.to_vec();
        }
        let Some(table_state) = self.engine.table(self.table) else {
            return vec![0.0; uqa_fusion::N_QUERY_FEATURES];
        };
        let idx_guard = table_state.inverted_index.read();
        let index_stats = idx_guard.stats();
        if let Some((field, query)) = first_text_signal(signals) {
            let analyzer = idx_guard.get_search_analyzer(&field);
            let terms = analyzer.analyze(&query);
            return uqa_fusion::extract_query_features(&index_stats, &terms, Some(&field)).to_vec();
        }
        vec![0.0; uqa_fusion::N_QUERY_FEATURES]
    }

    fn require_column(&self, field: &str) -> DriverResult<()> {
        if !self.engine.has_table(self.table) {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        }
        if !self.engine.table_has_column(self.table, field) {
            return Err(SQLError::UnknownColumn(field.to_string()));
        }
        Ok(())
    }

    fn require_vector_query(&self, field: &str, query_vector: &[f32]) -> DriverResult<()> {
        self.require_column(field)?;
        let expected_dimensions = match self.engine.column_type(self.table, field) {
            Some(ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions)) => {
                dimensions as usize
            }
            Some(column_type) => {
                return Err(SQLError::TypeMismatch(format!(
                    "vector search requires a VECTOR or TENSOR field, but {field:?} is {column_type:?}"
                )));
            }
            None => return Err(SQLError::UnknownColumn(field.to_string())),
        };
        if query_vector.len() != expected_dimensions {
            return Err(SQLError::TypeMismatch(format!(
                "vector query for {field:?} has {} dimensions, expected {expected_dimensions}",
                query_vector.len()
            )));
        }
        if query_vector.iter().any(|value| !value.is_finite()) {
            return Err(SQLError::TypeMismatch(format!(
                "vector query for {field:?} must contain only finite values"
            )));
        }
        let context = self
            .engine
            .snapshot_context(self.table)
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))?;
        if !context.vector_indexes.contains_key(field) {
            return Err(SQLError::Unsupported(format!(
                "vector field {field:?} has no physical vector index"
            )));
        }
        Ok(())
    }

    fn bridge_context(&self) -> DriverResult<uqa_operators::base::ExecutionContext> {
        if self.table.is_empty() {
            return Ok(uqa_operators::base::ExecutionContext::new());
        }
        self.engine
            .snapshot_context(self.table)
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))
    }

    fn facet_vector_inline(
        &self,
        vec_pl: &PostingList,
        facet_field: &str,
    ) -> DriverResult<PostingList> {
        use std::collections::BTreeMap;
        let state = self
            .engine
            .table(self.table)
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))?;
        if !self.engine.table_has_column(self.table, facet_field) {
            return Err(SQLError::UnknownColumn(facet_field.to_string()));
        }
        let snapshot = state.document_store.read().snapshot();
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for entry in vec_pl.entries() {
            if let Some(value) = snapshot.get_field(entry.doc_id, facet_field) {
                if !matches!(value, Value::Null) {
                    let key = match value {
                        Value::Str(s) => s,
                        Value::Int(n) => n.to_string(),
                        Value::Float(f) => format!("{f}"),
                        Value::Bool(b) => b.to_string(),
                        other => format!("{other:?}"),
                    };
                    *counts.entry(key).or_insert(0) += 1;
                }
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(counts.len());
        for (i, (value, count)) in counts.into_iter().enumerate() {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "_facet_field".to_string(),
                Value::Str(facet_field.to_string()),
            );
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert("_facet_count".to_string(), Value::Int(count as i64));
            entries.push(PostingEntry::new(
                i as DocId,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}

/// Replay a posting list that the [`EngineDriver`] has already
/// computed. Used by fusion / boolean wrappers that take
/// `Arc<dyn Operator>` signals: the driver pre-executes each child
/// node and hands the result over as a [`StaticPostingList`].
struct StaticPostingList {
    pl: PostingList,
}

fn lower_deep_conv(
    edge_label: Option<&str>,
    hop_weights: &[f64],
    direction: uqa_operators::DeepGraphDirection,
) -> DriverResult<uqa_ml::Layer> {
    if hop_weights.is_empty()
        || hop_weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
        || hop_weights.iter().sum::<f64>() <= 0.0
    {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Conv.hop_weights must be a non-empty finite non-negative vector with positive sum, got {hop_weights:?}"
        )));
    }
    Ok(uqa_ml::Layer::Conv {
        edge_label: edge_label.unwrap_or_default().to_string(),
        hop_weights: hop_weights.to_vec(),
        direction,
    })
}

fn lower_deep_pool(
    edge_label: Option<&str>,
    pool_size: usize,
    method: uqa_operators::DeepFusionPoolMethod,
    direction: uqa_operators::DeepGraphDirection,
) -> DriverResult<uqa_ml::Layer> {
    if pool_size == 0 {
        return Err(SQLError::TypeMismatch(
            "DeepFusion Pool.pool_size must be positive".to_string(),
        ));
    }
    let method = match method {
        uqa_operators::DeepFusionPoolMethod::Average => uqa_ml::DeepPoolMethod::Avg,
        uqa_operators::DeepFusionPoolMethod::Max => uqa_ml::DeepPoolMethod::Max,
    };
    Ok(uqa_ml::Layer::Pool {
        edge_label: edge_label.unwrap_or_default().to_string(),
        pool_size,
        method,
        direction,
    })
}

fn lower_deep_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
) -> DriverResult<uqa_ml::Layer> {
    let Some(expected_weights) = output_channels.checked_mul(input_channels) else {
        return Err(SQLError::TypeMismatch(
            "DeepFusion Dense dimensions overflow usize".to_string(),
        ));
    };
    if output_channels == 0
        || input_channels == 0
        || weights.len() != expected_weights
        || bias.len() != output_channels
        || weights.iter().chain(bias).any(|value| !value.is_finite())
    {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Dense requires positive dimensions, {expected_weights} weights, and {output_channels} biases; got {} weights and {} biases",
            weights.len(),
            bias.len()
        )));
    }
    Ok(uqa_ml::Layer::Dense {
        weights: weights.to_vec(),
        bias: bias.to_vec(),
        output_channels,
        input_channels,
    })
}

fn lower_deep_batch_norm(epsilon: f64) -> DriverResult<uqa_ml::Layer> {
    if !epsilon.is_finite() || epsilon <= 0.0 {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion BatchNorm.epsilon must be finite and positive, got {epsilon}"
        )));
    }
    Ok(uqa_ml::Layer::BatchNorm { epsilon })
}

fn lower_deep_dropout(probability: f64) -> DriverResult<uqa_ml::Layer> {
    if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Dropout.probability must be finite and in [0, 1], got {probability}"
        )));
    }
    Ok(uqa_ml::Layer::Dropout { p: probability })
}

fn deep_runtime_gating(gating: &GatingSpec) -> uqa_ml::Gating {
    match gating {
        GatingSpec::Softplus => uqa_ml::Gating::Softplus,
        GatingSpec::Pass => uqa_ml::Gating::None,
        GatingSpec::Sigmoid { .. } => uqa_ml::Gating::Sigmoid,
        GatingSpec::ReLU => uqa_ml::Gating::ReLU,
        GatingSpec::Swish => uqa_ml::Gating::Swish,
        GatingSpec::Gelu => uqa_ml::Gating::Gelu,
    }
}

#[derive(Default)]
struct GraphNeighborSnapshot {
    out: BTreeMap<u64, Vec<(String, u64)>>,
    incoming: BTreeMap<u64, Vec<(String, u64)>>,
}

impl GraphNeighborSnapshot {
    fn from_store(store: &uqa_graph::MemoryGraphStore, graph: &str) -> Self {
        use uqa_graph::GraphStore;

        let mut snapshot = Self::default();
        for edge in store.edges_in_graph(graph) {
            snapshot
                .out
                .entry(edge.source_id)
                .or_default()
                .push((edge.label.clone(), edge.target_id));
            snapshot
                .incoming
                .entry(edge.target_id)
                .or_default()
                .push((edge.label, edge.source_id));
        }
        snapshot
    }
}

impl uqa_operators::GraphNeighborLookup for GraphNeighborSnapshot {
    fn neighbors(
        &self,
        vertex: u64,
        label: &str,
        direction: uqa_operators::DeepGraphDirection,
    ) -> Vec<u64> {
        let mut result = Vec::new();
        let mut append = |edges: Option<&Vec<(String, u64)>>| {
            if let Some(edges) = edges {
                result.extend(
                    edges
                        .iter()
                        .filter(|(edge_label, _)| label.is_empty() || edge_label == label)
                        .map(|(_, neighbor)| *neighbor),
                );
            }
        };
        if matches!(
            direction,
            uqa_operators::DeepGraphDirection::Out | uqa_operators::DeepGraphDirection::Both
        ) {
            append(self.out.get(&vertex));
        }
        if matches!(
            direction,
            uqa_operators::DeepGraphDirection::In | uqa_operators::DeepGraphDirection::Both
        ) {
            append(self.incoming.get(&vertex));
        }
        result.sort_unstable();
        result.dedup();
        result
    }
}

fn static_operator(pl: PostingList) -> std::sync::Arc<dyn uqa_operators::Operator> {
    std::sync::Arc::new(StaticPostingList { pl })
}

fn numeric_score(value: &Value) -> f64 {
    match value {
        Value::Int(value) => *value as f64,
        Value::Float(value) => *value,
        _ => 0.0,
    }
}

fn graph_pattern_from_ir(pattern: &uqa_operators::GraphPatternIR) -> uqa_graph::GraphPattern {
    let mut converted = uqa_graph::GraphPattern::new();
    for vertex in &pattern.vertex_patterns {
        let mut converted_vertex = uqa_graph::VertexPattern::new(&vertex.variable);
        if let Some(label) = &vertex.label {
            converted_vertex =
                converted_vertex.with(uqa_graph::VertexPredicate::LabelEq(label.clone()));
        }
        for constraint in &vertex.constraints {
            converted_vertex =
                converted_vertex.with(uqa_graph::VertexPredicate::Custom(constraint.clone()));
        }
        converted = converted.add_vertex(converted_vertex);
    }
    for edge in &pattern.edge_patterns {
        let mut converted_edge = uqa_graph::EdgePattern::new(&edge.source_var, &edge.target_var);
        if let Some(label) = &edge.label {
            converted_edge = converted_edge.with_label(label);
        }
        for constraint in &edge.constraints {
            converted_edge =
                converted_edge.with(uqa_graph::EdgePredicate::Custom(constraint.clone()));
        }
        converted = converted.add_edge(converted_edge);
    }
    converted
}

fn parse_rpq(source: &str) -> DriverResult<uqa_graph::RegularPathExpr> {
    uqa_graph::parse_rpq(source)
        .map_err(|error| SQLError::TypeMismatch(format!("invalid RPQ {source:?}: {error}")))
}

fn temporal_filter_from_ir(
    filter: Option<&uqa_operators::TemporalFilterIR>,
) -> DriverResult<uqa_graph::TemporalFilter> {
    let Some(filter) = filter else {
        return Ok(uqa_graph::TemporalFilter::Any);
    };
    if filter.timestamp.is_some_and(f64::is_nan) {
        return Err(SQLError::TypeMismatch(
            "temporal timestamp cannot be NaN".to_string(),
        ));
    }
    if let Some((start, end)) = filter.time_range {
        if start.is_nan() || end.is_nan() || start > end {
            return Err(SQLError::TypeMismatch(format!(
                "temporal range must be ordered and non-NaN, got [{start}, {end}]"
            )));
        }
    }
    match (filter.timestamp, filter.time_range) {
        (Some(timestamp), Some((start, end))) => Ok(uqa_graph::TemporalFilter::TimestampAndRange(
            timestamp, start, end,
        )),
        (Some(timestamp), None) => Ok(uqa_graph::TemporalFilter::Timestamp(timestamp)),
        (None, Some((start, end))) => Ok(uqa_graph::TemporalFilter::Range(start, end)),
        (None, None) => Ok(uqa_graph::TemporalFilter::Any),
    }
}

fn restrict_result_to_source(result: &PostingList, source: &PostingList) -> PostingList {
    let source_by_id: BTreeMap<DocId, &Payload> = source
        .entries()
        .iter()
        .map(|entry| (entry.doc_id, &entry.payload))
        .collect();
    let entries = result
        .entries()
        .iter()
        .filter_map(|entry| {
            let source_payload = source_by_id.get(&entry.doc_id)?;
            let mut payload = entry.payload.clone();
            for (field, value) in &source_payload.fields {
                payload
                    .fields
                    .entry(field.clone())
                    .or_insert_with(|| value.clone());
            }
            Some(PostingEntry::new(entry.doc_id, payload))
        })
        .collect();
    PostingList::from_sorted_unchecked(entries)
}

fn require_graph_name(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    let mut names = BTreeSet::new();
    collect_graph_names(tree, &mut names);
    match names.len() {
        1 => Ok(names.into_iter().next().expect("one graph name")),
        0 => Err(SQLError::TypeMismatch(format!(
            "{context} does not identify a graph"
        ))),
        _ => Err(SQLError::TypeMismatch(format!(
            "{context} spans multiple graphs: {names:?}"
        ))),
    }
}

fn require_text_field(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    first_text_field(tree)
        .ok_or_else(|| SQLError::TypeMismatch(format!("{context} does not identify a text field")))
}

fn require_vector_field(tree: &OperatorTree, context: &str) -> DriverResult<String> {
    first_vector_field(tree).ok_or_else(|| {
        SQLError::TypeMismatch(format!("{context} does not identify a vector field"))
    })
}

fn require_shared_structured_field(
    left: &OperatorTree,
    right: &OperatorTree,
    context: &str,
) -> DriverResult<(String, String)> {
    let left = first_structured_field(left).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{context}.left does not identify a structured field"
        ))
    })?;
    let right = first_structured_field(right).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{context}.right does not identify a structured field"
        ))
    })?;
    Ok((left, right))
}

fn require_shared_vector_field(
    left: &OperatorTree,
    right: &OperatorTree,
    context: &str,
) -> DriverResult<(String, String)> {
    Ok((
        require_vector_field(left, &format!("{context}.left"))?,
        require_vector_field(right, &format!("{context}.right"))?,
    ))
}

fn first_text_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::Term { field, .. } => field.clone(),
        OperatorTree::Score { field, .. }
        | OperatorTree::BayesianScore {
            field: Some(field), ..
        } => Some(field.clone()),
        OperatorTree::MultiFieldSearch { fields, .. } if fields.len() == 1 => {
            fields.first().cloned()
        }
        OperatorTree::Opaque { meta, .. } => match meta.get("field") {
            Some(Value::Str(field)) => Some(field.clone()),
            _ => first_child(tree).and_then(first_text_field),
        },
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_text_field),
        _ => first_child(tree).and_then(first_text_field),
    }
}

fn first_vector_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::VectorSimilarity { field, .. } | OperatorTree::KNN { field, .. } => {
            Some(field.clone())
        }
        OperatorTree::GraphEmbedding { .. } => Some("_embedding".to_string()),
        OperatorTree::Opaque { kind, meta, .. } if kind == "calibrated_vector_match" => {
            match meta.get("field") {
                Some(Value::Str(field)) => Some(field.clone()),
                _ => None,
            }
        }
        OperatorTree::HybridTextVector { vector_op, .. }
        | OperatorTree::SemanticFilter { vector_op, .. }
        | OperatorTree::FacetVector { vector_op, .. } => first_vector_field(vector_op),
        OperatorTree::VectorExclusion { positive, negative } => {
            first_vector_field(positive).or_else(|| first_vector_field(negative))
        }
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_vector_field),
        _ => first_child(tree).and_then(first_vector_field),
    }
}

fn first_structured_field(tree: &OperatorTree) -> Option<String> {
    match tree {
        OperatorTree::Filter { field, .. }
        | OperatorTree::Facet { field, .. }
        | OperatorTree::IndexScan { field, .. }
        | OperatorTree::Aggregate { field, .. } => Some(field.clone()),
        OperatorTree::GroupBy { group_field, .. } => Some(group_field.clone()),
        OperatorTree::Opaque { meta, .. } => match meta.get("field") {
            Some(Value::Str(field)) => Some(field.clone()),
            _ => first_child(tree).and_then(first_structured_field),
        },
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().find_map(first_structured_field),
        _ => first_child(tree).and_then(first_structured_field),
    }
}

fn first_child(tree: &OperatorTree) -> Option<&OperatorTree> {
    match tree {
        OperatorTree::Filter {
            source: Some(source),
            ..
        }
        | OperatorTree::Facet {
            source: Some(source),
            ..
        }
        | OperatorTree::Score { source, .. }
        | OperatorTree::BayesianScore { source, .. }
        | OperatorTree::Complement(source)
        | OperatorTree::CosineProbability(source)
        | OperatorTree::ProbNot { signal: source, .. }
        | OperatorTree::SparseThreshold { source, .. }
        | OperatorTree::VertexAggregation { source, .. }
        | OperatorTree::MessagePassing { source }
        | OperatorTree::GraphEmbedding { source }
        | OperatorTree::GroupBy { source, .. }
        | OperatorTree::SemanticFilter { source, .. }
        | OperatorTree::Aggregate {
            source: Some(source),
            ..
        }
        | OperatorTree::GraphJoin { left: source, .. }
        | OperatorTree::TextSimilarityJoin { left: source, .. }
        | OperatorTree::VectorSimilarityJoin { left: source, .. }
        | OperatorTree::HybridJoin { left: source, .. }
        | OperatorTree::CrossParadigmJoin { left: source, .. }
        | OperatorTree::HybridTextVector {
            term_op: source, ..
        }
        | OperatorTree::VectorExclusion {
            positive: source, ..
        }
        | OperatorTree::FacetVector {
            vector_op: source, ..
        } => Some(source),
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children)
        | OperatorTree::Opaque { children, .. } => children.first(),
        OperatorTree::LogOddsFusion { signals, .. }
        | OperatorTree::ProbBoolFusion { signals, .. }
        | OperatorTree::AttentionFusion { signals, .. }
        | OperatorTree::LearnedFusion { signals, .. } => signals.first(),
        OperatorTree::MultiStage { stages } => stages.first().map(|stage| &stage.child),
        OperatorTree::ProgressiveFusion { stages, .. } => stages.first().map(|stage| &stage.signal),
        OperatorTree::DeepFusion { layers, .. } => layers.iter().find_map(|layer| match layer {
            uqa_operators::DeepFusionLayer::Signal { signals } => signals.first(),
            _ => None,
        }),
        _ => None,
    }
}

fn collect_graph_names(tree: &OperatorTree, names: &mut BTreeSet<String>) {
    tree.visit(&mut |node| {
        let graph = match node {
            OperatorTree::Traverse { graph, .. }
            | OperatorTree::PatternMatch { graph, .. }
            | OperatorTree::RegularPathQuery { graph, .. }
            | OperatorTree::WeightedPathQuery { graph, .. }
            | OperatorTree::GraphJoin { graph, .. }
            | OperatorTree::PageRank { graph }
            | OperatorTree::HITS { graph }
            | OperatorTree::BetweennessCentrality { graph }
            | OperatorTree::TemporalTraverse { graph, .. }
            | OperatorTree::TemporalPatternMatch { graph, .. } => Some(graph),
            _ => None,
        };
        if let Some(graph) = graph {
            names.insert(graph.clone());
        }
    });
}

impl uqa_operators::base::Operator for StaticPostingList {
    fn execute(&self, _ctx: &uqa_operators::base::ExecutionContext) -> PostingList {
        self.pl.clone()
    }
}

/// Walk a slice of fusion signals and find the first text-bearing
/// node so attention's query-feature extractor has a query to score
/// against. Returns `(field, query)` of the first matching `Term` (or
/// `Score`-wrapped `Term`); falls back to `None` when no text signal
/// is present in the fusion args.
fn first_text_signal(signals: &[OperatorTree]) -> Option<(String, String)> {
    for sig in signals {
        if let Some(pair) = find_text_in_tree(sig) {
            return Some(pair);
        }
    }
    None
}

fn find_text_in_tree(tree: &OperatorTree) -> Option<(String, String)> {
    match tree {
        OperatorTree::Term { query, field, .. } => field.clone().map(|f| (f, query.clone())),
        OperatorTree::Opaque { kind, meta, .. } if kind == "bayesian_match_with_prior" => {
            let Some(Value::Str(field)) = meta.get("field") else {
                return None;
            };
            let Some(Value::Str(query)) = meta.get("query") else {
                return None;
            };
            Some((field.clone(), query.clone()))
        }
        OperatorTree::Score {
            source,
            query_terms,
            field,
            ..
        } => {
            // Score wraps a Term; flatten the underlying query string
            // back out by joining the analyzed terms with spaces.
            if let Some(inner) = find_text_in_tree(source) {
                return Some(inner);
            }
            Some((field.clone(), query_terms.join(" ")))
        }
        OperatorTree::Filter {
            source: Some(s), ..
        } => find_text_in_tree(s),
        OperatorTree::Composed(parts)
        | OperatorTree::Intersect(parts)
        | OperatorTree::Union(parts) => parts.iter().find_map(find_text_in_tree),
        OperatorTree::Complement(inner)
        | OperatorTree::CosineProbability(inner)
        | OperatorTree::BayesianScore { source: inner, .. } => find_text_in_tree(inner),
        _ => None,
    }
}

/// Combine a vector of per-signal posting lists into a single fused
/// posting list. `fuse` receives the per-signal probability vector
/// for one document and returns the fused score. Mirrors the
/// `collect_score_maps` + per-doc loop in
/// `uqa_operators::fusion_wrappers`.
fn fuse_signals_with<F>(posting_lists: &[PostingList], fuse: F) -> PostingList
where
    F: Fn(&[f64]) -> f64,
{
    use std::collections::{BTreeMap, BTreeSet};
    let mut maps: Vec<BTreeMap<DocId, f64>> = Vec::with_capacity(posting_lists.len());
    let mut all_ids: BTreeSet<DocId> = BTreeSet::new();
    for pl in posting_lists {
        let mut m: BTreeMap<DocId, f64> = BTreeMap::new();
        for entry in pl {
            m.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        maps.push(m);
    }
    let total = all_ids.len();
    if total == 0 {
        return PostingList::new();
    }
    let defaults: Vec<f64> = maps
        .iter()
        .map(|m| uqa_operators::hybrid::coverage_based_default(m.len(), total, 0.01))
        .collect();
    let mut entries: Vec<PostingEntry> = Vec::with_capacity(total);
    for doc_id in all_ids {
        let probs: Vec<f64> = maps
            .iter()
            .enumerate()
            .map(|(j, m)| *m.get(&doc_id).unwrap_or(&defaults[j]))
            .collect();
        let fused = fuse(&probs);
        entries.push(PostingEntry::new(
            doc_id,
            Payload {
                score: fused,
                ..Default::default()
            },
        ));
    }
    PostingList::from_sorted_unchecked(entries)
}

fn scored_to_posting_list(scored: &[ScoredEntry]) -> PostingList {
    let mut entries: Vec<PostingEntry> = scored
        .iter()
        .map(|e| PostingEntry::new(e.doc_id, Payload::with_score(e.score)))
        .collect();
    entries.sort_by_key(|e| e.doc_id);
    PostingList::from_sorted_unchecked(entries)
}

fn posting_list_to_scored(pl: &PostingList) -> Vec<ScoredEntry> {
    pl.entries()
        .iter()
        .map(|e| ScoredEntry {
            doc_id: e.doc_id,
            score: e.payload.score,
        })
        .collect()
}

fn sparse_threshold_inline(source: &PostingList, threshold: f64) -> PostingList {
    let entries = source
        .iter()
        .filter_map(|entry| {
            let adjusted = entry.payload.score - threshold;
            if adjusted > 0.0 {
                Some(PostingEntry::new(
                    entry.doc_id,
                    Payload {
                        positions: entry.payload.positions.clone(),
                        score: adjusted,
                        fields: entry.payload.fields.clone(),
                    },
                ))
            } else {
                None
            }
        })
        .collect();
    PostingList::from_unsorted(entries)
}

/// Lower a WHERE expression and run [`QueryOptimizer`] over the
/// resulting tree without executing it. Useful for tests and
/// `EXPLAIN`-style diagnostics that want to inspect the rewritten
/// shape before any posting list is materialised.
#[must_use]
pub fn optimised_tree_for(
    engine: &Engine,
    table: &str,
    where_expr: &ScalarExpr,
    params: &[SQLParam],
) -> Option<OperatorTree> {
    let tree = lower_where(where_expr, params)?;
    Some(engine_query_optimizer(engine, table, &tree).optimize(tree))
}

/// The "lower -> optimise -> execute" pipeline. `Some(rows)` when the
/// WHERE expression maps cleanly onto the operator tree; `None` keeps the
/// predicate in the enclosing relational filter node. Any engine-side failure
/// returned by the helpers it re-uses bubbles up as `Err`.
pub fn run_optimised(
    engine: &Engine,
    table: &str,
    where_expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
    let Some(expr) = where_expr else {
        return Ok(None);
    };
    let Some(tree) = lower_where(expr, params) else {
        return Ok(None);
    };
    let pl = expect_posting_output(
        execute_operator_tree(engine, table, params, &tree)?,
        "SQL WHERE",
    )?;
    Ok(Some(posting_list_to_scored(&pl)))
}

/// Optimise and execute an already-lowered tree through the same
/// planner/runtime boundary used by SQL `WHERE` lowering. Graph table
/// functions use this entry point too, so they do not maintain a
/// second physical dispatch implementation for nodes represented by
/// [`OperatorTree`].
pub(crate) fn execute_operator_tree(
    engine: &Engine,
    table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    let optimized = engine_query_optimizer(engine, table, tree).optimize(tree.clone());
    let driver = EngineDriver::new(engine, table, params);
    let mut executor = PlanExecutor::new(&driver);
    executor.execute(&optimized)
}

fn engine_query_optimizer(engine: &Engine, table: &str, tree: &OperatorTree) -> QueryOptimizer {
    let candidates = engine_index_candidates(engine, table, tree);
    QueryOptimizer::new()
        .with_row_count(engine.table_doc_count(table))
        .with_index_candidates(candidates, table)
}

fn engine_index_candidates(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> Vec<IndexScanCandidate> {
    if table.is_empty() || !engine.has_table(table) {
        return Vec::new();
    }
    let resolved_table = engine
        .resolve_table_name(table)
        .unwrap_or_else(|| table.to_string());
    let mut indexes_by_field = BTreeMap::new();
    for index in engine.list_catalog_indexes() {
        if index.table_name != resolved_table || !index.index_type.eq_ignore_ascii_case("btree") {
            continue;
        }
        let Ok(columns) = serde_json::from_str::<Vec<String>>(&index.columns_json) else {
            continue;
        };
        if let Some(field) = columns.first() {
            indexes_by_field
                .entry(field.clone())
                .or_insert(index.name.clone());
        }
    }

    let mut candidates = Vec::new();
    tree.visit(&mut |node| {
        let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = node
        else {
            return;
        };
        let Some(index_name) = indexes_by_field.get(field) else {
            return;
        };
        let Some(matches) = engine.value_index_scan(table, field, predicate) else {
            return;
        };
        let cardinality = matches.len() as f64;
        let scan_cost = match predicate {
            Predicate::Equals(_) => 1.0 + cardinality * 0.1,
            _ => cardinality.max(1.0),
        };
        candidates.push(IndexScanCandidate {
            index_name: index_name.clone(),
            table_name: table.to_string(),
            field: field.clone(),
            predicate: predicate.clone(),
            scan_cost,
        });
    });
    candidates
}

pub(crate) fn expect_posting_output(
    output: OperatorOutput,
    context: &str,
) -> DriverResult<PostingList> {
    match output {
        OperatorOutput::Posting(result) => Ok(result),
        OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
            "{context} requires single-document rows, but the physical plan produced join tuples"
        ))),
    }
}

/// Combine the corpus priors reported by fusion signals into the single
/// fusion-level prior: the mean of their logits. Every signal estimates
/// the same corpus-level P(relevant), so averaging in log-odds space
/// yields one prior no matter how many signals report it.
/// Number of score-contributing text terms in a bound BM25 query tree.
/// Set operations merge payloads by summing scores, so the raw query
/// score scales with this count and the calibration must be translated
/// to it. Complements filter without contributing score.
fn scored_term_count(tree: &OperatorTree) -> usize {
    match tree {
        OperatorTree::Term { .. } => 1,
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().map(scored_term_count).sum(),
        OperatorTree::Filter { source, .. } => source.as_deref().map_or(0, scored_term_count),
        OperatorTree::BayesianScore { source, .. } | OperatorTree::Score { source, .. } => {
            scored_term_count(source)
        }
        _ => 0,
    }
}

pub(crate) fn combine_signal_priors(priors: &[f64]) -> Option<f64> {
    if priors.is_empty() {
        return None;
    }
    let mean_logit = priors
        .iter()
        .map(|rate| uqa_scoring::logit(*rate))
        .sum::<f64>()
        / priors.len() as f64;
    Some(uqa_scoring::sigmoid(mean_logit))
}

/// Execute an `fts_match(field, query)` call as a fusion signal:
/// prior-free evidence rows plus the corpus prior the signal would
/// otherwise fold in. Returns `Ok(None)` when the query cannot be
/// lowered (e.g. non-constant arguments), in which case the caller
/// falls back to the opaque posterior form. Execution failures remain
/// errors.
pub(crate) fn run_fts_fusion_signal(
    engine: &Engine,
    table: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Option<ScoredFusionSignal>, SQLError> {
    let Some(tree) = lower_calibrated_signal("fts_match", args, params) else {
        return Ok(None);
    };
    let driver = EngineDriver::new(engine, table, params);
    let (pl, prior) = driver.execute_fusion_signal(&tree)?;
    Ok(Some((posting_list_to_scored(&pl), prior)))
}

// `eval_path` lives in storage; expose a shim so we don't pull in the
// trait at the lowering layer just for this helper.
#[allow(dead_code)]
fn lookup_path(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let mut current = value.clone();
    for seg in path {
        current = match (current, seg) {
            (Value::Map(m), PathSegment::Key(k)) => m.get(k)?.clone(),
            (Value::List(items), PathSegment::Index(i)) => items.get(*i)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}
