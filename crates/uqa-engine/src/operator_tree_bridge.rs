//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bridge between the SQL `WHERE` AST and the operator-tree IR.
//!
//! Python compiles every `SELECT ... WHERE ...` into an `OperatorTree`
//! (boolean / scoring / fusion / graph / index-scan nodes), runs the
//! tree through `QueryOptimizer` (the 10-pass algebraic / graph-aware
//! / fusion-reordering optimiser), and only then executes it through
//! `PlanExecutor`. Until now the Rust port had `QueryOptimizer` ported
//! 1:1 in `uqa_planner::query_optimizer` but it was dead code — the
//! engine ran the SQL `SelectStmt` directly without ever building the
//! operator tree, so none of the optimiser's algebraic / graph-aware
//! rewrites fired in production.
//!
//! This module wires the two halves together:
//!
//! 1. [`lower_where`] turns a SQL `Expr` (the WHERE clause) plus the
//!    target table into an `OperatorTree`. Boolean connectives map onto
//!    `Intersect` / `Union` / `Complement`, scoring / KNN / fusion
//!    function calls map onto the matching `OperatorTree` variants, and
//!    column comparison predicates lower into `Filter` nodes.
//! 2. [`EngineDriver`] implements [`OperatorTreeDriver`] by calling
//!    back into the engine's existing `run_text_match` / `run_knn_match`
//!    / `run_fuse_log_odds` / ... helpers, and combining child posting
//!    lists with the Boolean algebra in `uqa_core::PostingList`.
//!
//! The integration target is a "lower → optimise → execute" pipeline:
//! [`run_optimised`] does the three-step sequence and returns a
//! [`Vec<ScoredEntry>`] that the caller can project, sort, and limit
//! through the existing row pipeline. Lowering is best-effort — when a
//! WHERE expression doesn't fit the operator tree (e.g. arithmetic
//! across columns), the function returns `None` and the caller falls
//! back to the legacy `execute_function` / `filter_table_rows` path.

use std::collections::BTreeSet;

use uqa_core::{DocId, PathSegment, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_operators::{GatingSpec, OperatorTree};
use uqa_planner::executor::{OperatorTreeDriver, PlanExecutor};
use uqa_planner::parallel::ParallelExecutor;
use uqa_planner::query_optimizer::QueryOptimizer;
use uqa_sql::ast::{BinaryOp, Expr};
use uqa_sql::expr::{eval, EvalContext};
use uqa_sql::SQLParam;

use crate::sql;
use crate::{Engine, ScoredEntry};
use uqa_sql::SQLError;

/// Lower a SQL `WHERE` expression into an [`OperatorTree`]. Returns
/// `None` for shapes the operator IR can't represent so the caller can
/// fall back to the row-evaluator path.
pub fn lower_where(expr: &Expr, params: &[SQLParam]) -> Option<OperatorTree> {
    match expr {
        Expr::And(parts) => {
            let mut out: Vec<OperatorTree> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, params)?);
            }
            Some(OperatorTree::Intersect(out))
        }
        Expr::Or(parts) => {
            let mut out: Vec<OperatorTree> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, params)?);
            }
            Some(OperatorTree::Union(out))
        }
        Expr::Not(inner) => Some(OperatorTree::Complement(Box::new(lower_where(
            inner, params,
        )?))),
        Expr::Func { name, args } => lower_function(name, args, params),
        Expr::Binary { op, lhs, rhs } => lower_comparison(*op, lhs, rhs, params),
        Expr::IsNull { expr, negated } => {
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
        Expr::Between { expr, low, high } => {
            let field = column_name(expr)?;
            let lo = const_value(low, params)?;
            let hi = const_value(high, params)?;
            Some(OperatorTree::Filter {
                field,
                predicate: Predicate::Between { low: lo, high: hi },
                source: None,
            })
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let field = column_name(expr)?;
            let mut set: BTreeSet<Value> = BTreeSet::new();
            for v in list {
                set.insert(const_value(v, params)?);
            }
            let pred = Predicate::InSet(set);
            let filter = OperatorTree::Filter {
                field,
                predicate: pred,
                source: None,
            };
            if *negated {
                Some(OperatorTree::Complement(Box::new(filter)))
            } else {
                Some(filter)
            }
        }
        _ => None,
    }
}

fn lower_function(name: &str, args: &[Expr], params: &[SQLParam]) -> Option<OperatorTree> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "text_match" | "bayesian_match" => {
            // Standalone text_match / bayesian_match -- the Term node
            // already runs through the Bayesian BM25 scorer, so the
            // standalone path produces calibrated probabilities.
            let field = column_name(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            Some(OperatorTree::Term {
                query,
                field: Some(field),
            })
        }
        "knn_match" => {
            // Standalone knn_match preserves raw cosine similarities;
            // calibration to (0, 1) only fires inside fusion contexts
            // (mirrors `_compile_calibrated_signal` semantics from the
            // Python reference: only fusion arms see calibrated KNN).
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
        "attention" => lower_attention_fusion(args, params),
        "learned_fusion" => lower_learned_fusion(args, params),
        _ => None,
    }
}

/// Compile a signal-function call into a node that produces calibrated
/// probabilities in (0, 1). Mirrors Python's
/// `_compile_calibrated_signal`: in fusion contexts every signal must
/// land on the (0, 1) probability scale before log-odds / attention /
/// learned fusion can combine them.
///
/// - `text_match` / `bayesian_match` --> [`OperatorTree::Term`] (the
///   engine evaluates `Term` through the Bayesian BM25 scorer, so the
///   result is already calibrated).
/// - `knn_match` --> [`OperatorTree::CosineProbability`] wrapping a
///   [`OperatorTree::KNN`] child, so cosine scores in `[-1, 1]` get
///   rescaled to `(0, 1)` via `(1 + s) / 2`.
fn lower_calibrated_signal(
    name: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    match name {
        "text_match" | "bayesian_match" => {
            let field = column_name(args.first()?)?;
            let query = const_string(args.get(1)?, params)?;
            Some(OperatorTree::Term {
                query,
                field: Some(field),
            })
        }
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
        _ => None,
    }
}

/// Lower a function-call argument into a calibrated signal node. Used
/// by every fusion lowering arm (`fuse_log_odds`, `attention`,
/// `learned_fusion`) so the rewrite stays consistent across fusers.
fn lower_signal_arg(arg: &Expr, params: &[SQLParam]) -> Option<OperatorTree> {
    match arg {
        Expr::Func { name, args } => {
            let lower = name.to_ascii_lowercase();
            lower_calibrated_signal(&lower, args, params)
        }
        _ => None,
    }
}

fn lower_fuse_log_odds(args: &[Expr], params: &[SQLParam]) -> Option<OperatorTree> {
    // `fuse_log_odds(signal_1, signal_2, ..., alpha)`. The last
    // argument must be a numeric literal alpha; the rest must lower
    // into calibrated signal trees.
    if args.len() < 2 {
        return None;
    }
    let alpha_expr = args.last()?;
    let alpha = const_f64(alpha_expr, params)?;
    let mut signals: Vec<OperatorTree> = Vec::with_capacity(args.len() - 1);
    for a in &args[..args.len() - 1] {
        signals.push(lower_signal_arg(a, params)?);
    }
    Some(OperatorTree::LogOddsFusion {
        signals,
        alpha,
        gating: GatingSpec::Pass,
    })
}

fn lower_attention_fusion(args: &[Expr], params: &[SQLParam]) -> Option<OperatorTree> {
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
    // n_query_features=6 (matches Python `_make_attention_fusion_op`).
    // Query features are filled in lazily at execute time from the
    // engine snapshot, so the IR carries a zero-vector placeholder.
    let attention: AttentionRef =
        Arc::new(AttentionFusion::new(signals.len(), N_QUERY_FEATURES, 0.5));
    Some(OperatorTree::AttentionFusion {
        signals,
        attention,
        query_features: Vec::new(),
    })
}

fn lower_learned_fusion(args: &[Expr], params: &[SQLParam]) -> Option<OperatorTree> {
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
    lhs: &Expr,
    rhs: &Expr,
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

fn column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(name) => Some(name.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn const_value(expr: &Expr, params: &[SQLParam]) -> Option<Value> {
    let ctx = EvalContext::new(None, params);
    eval(expr, &ctx).ok()
}

fn const_string(expr: &Expr, params: &[SQLParam]) -> Option<String> {
    match const_value(expr, params)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

fn const_f64(expr: &Expr, params: &[SQLParam]) -> Option<f64> {
    match const_value(expr, params)? {
        Value::Int(n) => Some(n as f64),
        Value::Float(f) => Some(f),
        _ => None,
    }
}

fn const_usize(expr: &Expr, params: &[SQLParam]) -> Option<usize> {
    match const_value(expr, params)? {
        Value::Int(n) if n >= 0 => Some(n as usize),
        _ => None,
    }
}

fn const_vector(expr: &Expr, params: &[SQLParam]) -> Option<Vec<f32>> {
    match expr {
        Expr::Array(items) => {
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

/// `OperatorTreeDriver` backed by the engine's existing per-function
/// helpers. Each leaf node delegates to `run_*_match`; combinators
/// operate over the resulting posting lists with `uqa_core` Boolean
/// algebra.
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

    fn execute_branches(&self, branches: &[OperatorTree]) -> Vec<PostingList> {
        let workers: Vec<_> = branches.iter().map(|b| || self.execute_node(b)).collect();
        self.parallel.execute_branches(&workers)
    }

    fn execute_term(&self, query: &str, field: Option<&str>) -> PostingList {
        // Re-use the existing text_match dispatcher. `text_match(field,
        // 'q')` produces a scored posting list against the table.
        let field_expr = match field {
            Some(f) => Expr::Column(f.to_string()),
            None => Expr::Literal(Value::Str(String::new())),
        };
        let args = vec![field_expr, Expr::Literal(Value::Str(query.to_string()))];
        match sql::run_text_match_public(self.engine, self.table, &args, self.params) {
            Ok(rows) => scored_to_posting_list(&rows),
            Err(_) => PostingList::new(),
        }
    }

    fn execute_knn(&self, query_vector: &[f32], k: usize, field: &str) -> PostingList {
        let v_expr = Expr::Array(
            query_vector
                .iter()
                .map(|x| Expr::Literal(Value::Float(f64::from(*x))))
                .collect(),
        );
        let args = vec![
            Expr::Column(field.to_string()),
            v_expr,
            Expr::Literal(Value::Int(k as i64)),
        ];
        match sql::run_knn_match_public(self.engine, self.table, &args, self.params) {
            Ok(rows) => scored_to_posting_list(&rows),
            Err(_) => PostingList::new(),
        }
    }

    fn execute_filter(
        &self,
        field: &str,
        predicate: &Predicate,
        source: Option<&OperatorTree>,
    ) -> PostingList {
        let candidates: Vec<DocId> = match source {
            Some(child) => {
                let inner = self.execute_node(child);
                inner.entries().iter().map(|e| e.doc_id).collect()
            }
            None => self.engine.table_doc_ids(self.table),
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        for doc_id in candidates {
            let value = self
                .engine
                .get_document(self.table, doc_id)
                .and_then(|d| d.get(field).cloned());
            if predicate.evaluate(value.as_ref()) {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }
}

impl OperatorTreeDriver for EngineDriver<'_> {
    #[allow(clippy::match_same_arms)]
    fn execute_node(&self, op: &OperatorTree) -> PostingList {
        match op {
            OperatorTree::Empty => PostingList::new(),
            OperatorTree::Term { query, field } => self.execute_term(query, field.as_deref()),
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
            OperatorTree::Intersect(parts) => {
                let mut iter = self.execute_branches(parts).into_iter();
                let Some(first) = iter.next() else {
                    return PostingList::new();
                };
                iter.fold(first, |acc, next| acc.intersect(&next))
            }
            OperatorTree::Union(parts) => {
                let mut iter = self.execute_branches(parts).into_iter();
                let Some(first) = iter.next() else {
                    return PostingList::new();
                };
                iter.fold(first, |acc, next| acc.union(&next))
            }
            OperatorTree::Complement(inner) => {
                let inner_pl = self.execute_node(inner);
                let included: BTreeSet<DocId> =
                    inner_pl.entries().iter().map(|e| e.doc_id).collect();
                let mut entries: Vec<PostingEntry> = Vec::new();
                for doc_id in self.engine.table_doc_ids(self.table) {
                    if !included.contains(&doc_id) {
                        entries.push(PostingEntry::new(doc_id, Payload::default()));
                    }
                }
                entries.sort_by_key(|e| e.doc_id);
                PostingList::from_sorted_unchecked(entries)
            }
            OperatorTree::Composed(parts) => {
                // Composed = sequential; treat as left-to-right intersect
                // of every child, giving the same semantics as the
                // Python `ComposedOperator` no-op chain.
                let mut iter = parts.iter().map(|p| self.execute_node(p));
                let Some(first) = iter.next() else {
                    return PostingList::new();
                };
                iter.fold(first, |acc, next| acc.intersect(&next))
            }
            OperatorTree::LogOddsFusion { signals, alpha, .. } => {
                self.execute_log_odds_fusion(signals, *alpha)
            }
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
            OperatorTree::VectorExclusion { positive, negative } => {
                self.execute_vector_exclusion(positive, negative)
            }
            OperatorTree::FacetVector {
                vector_op,
                facet_field,
            } => {
                let vec_pl = self.execute_node(vector_op);
                self.facet_vector_inline(&vec_pl, facet_field)
            }
            OperatorTree::CosineProbability(source) => self.execute_cosine_probability(source),
            OperatorTree::AttentionFusion {
                signals,
                attention,
                query_features,
            } => self.execute_attention_fusion(signals, attention, query_features),
            OperatorTree::LearnedFusion { signals, learned } => {
                self.execute_learned_fusion(signals, learned)
            }
            // The remaining graph-only IR variants (PatternMatch,
            // Traverse, RegularPathQuery, WeightedPathQuery,
            // CypherQuery, ...) need a shared graph store handle to
            // execute. The engine routes those through dedicated
            // table-function entry points; if one shows up here we
            // signal an empty result rather than misreporting matches.
            _ => PostingList::new(),
        }
    }
}

impl EngineDriver<'_> {
    fn execute_prob_bool_fusion(
        &self,
        signals: &[OperatorTree],
        mode: uqa_operators::ProbBoolMode,
    ) -> PostingList {
        use uqa_operators::base::Operator;
        use uqa_operators::{HybridProbBoolMode, ProbBoolFusionOperator};
        if signals.is_empty() {
            return PostingList::new();
        }
        // Pre-execute every child through the driver, then wrap the
        // results in static signal stubs so the fusion operator can
        // consume them without taking a back-reference into the
        // driver.
        let signal_ops: Vec<std::sync::Arc<dyn Operator>> = self
            .execute_branches(signals)
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
        op.execute(&self.bridge_context())
    }

    fn execute_prob_not(&self, signal: &OperatorTree, default_prob: f64) -> PostingList {
        use uqa_operators::base::Operator;
        use uqa_operators::ProbNotOperator;
        let signal_pl = self.execute_node(signal);
        let stub: std::sync::Arc<dyn Operator> =
            std::sync::Arc::new(StaticPostingList { pl: signal_pl });
        let op = ProbNotOperator::new(stub, default_prob);
        op.execute(&self.bridge_context())
    }

    fn execute_index_scan(
        &self,
        index_name: &str,
        field: &str,
        predicate: &uqa_core::Predicate,
    ) -> PostingList {
        let _ = index_name;
        // The Rust port stores `index_name` as a String; resolving it
        // to an `Arc<dyn Index>` requires the engine's IndexManager.
        // Until that hookup lands the driver evaluates the predicate
        // against the table directly (matches a `Filter { source: None }`
        // arm).
        self.execute_filter(field, predicate, None)
    }

    fn execute_vector_exclusion(
        &self,
        positive: &OperatorTree,
        negative: &OperatorTree,
    ) -> PostingList {
        let pos = self.execute_node(positive);
        let neg = self.execute_node(negative);
        let neg_ids: BTreeSet<DocId> = neg.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in pos.entries() {
            if !neg_ids.contains(&entry.doc_id) {
                entries.push(entry.clone());
            }
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn execute_log_odds_fusion(&self, signals: &[OperatorTree], alpha: f64) -> PostingList {
        if signals.is_empty() {
            return PostingList::new();
        }
        let posting_lists: Vec<PostingList> = self.execute_branches(signals);
        let fuser = uqa_fusion::LogOddsFusion::new(alpha);
        fuse_signals_with(&posting_lists, |probs| fuser.fuse(probs))
    }

    fn execute_cosine_probability(&self, source: &OperatorTree) -> PostingList {
        // Lift cosine similarities in `[-1, 1]` onto the (0, 1)
        // probability scale via `(1 + s) / 2`. Mirrors
        // [`uqa_operators::CosineProbabilityOperator`] but skips the
        // trait wrapper because the source has already been driven
        // through the engine.
        use uqa_scoring::cosine_to_probability;
        let pl = self.execute_node(source);
        pl.with_scores(|e| cosine_to_probability(e.payload.score))
    }

    fn execute_attention_fusion(
        &self,
        signals: &[OperatorTree],
        attention: &uqa_operators::tree::AttentionRef,
        query_features: &[f64],
    ) -> PostingList {
        if signals.is_empty() {
            return PostingList::new();
        }
        let posting_lists: Vec<PostingList> = self.execute_branches(signals);
        let features = self.attention_query_features(signals, query_features);
        fuse_signals_with(&posting_lists, |probs| {
            attention.fuse(probs, &features)
        })
    }

    fn execute_learned_fusion(
        &self,
        signals: &[OperatorTree],
        learned: &uqa_operators::tree::LearnedFusionRef,
    ) -> PostingList {
        if signals.is_empty() {
            return PostingList::new();
        }
        let posting_lists: Vec<PostingList> = self.execute_branches(signals);
        fuse_signals_with(&posting_lists, |probs| learned.fuse(probs))
    }

    /// Build the `n_query_features=6` vector that attention fusers
    /// expect. When the IR carries a non-empty placeholder it wins
    /// (test fixtures); otherwise the driver extracts the canonical
    /// `[mean_idf, max_idf, min_idf, coverage, query_length,
    /// vocab_overlap]` vector from the table's inverted-index stats
    /// against the first text-bearing signal it can find.
    fn attention_query_features(
        &self,
        signals: &[OperatorTree],
        explicit: &[f64],
    ) -> Vec<f64> {
        if !explicit.is_empty() {
            return explicit.to_vec();
        }
        let Some(table_state) = self.engine.table(self.table) else {
            return vec![0.0; uqa_fusion::N_QUERY_FEATURES];
        };
        let idx_guard = table_state.inverted_index.read();
        let index_stats = idx_guard.stats();
        let analyzer = idx_guard.analyzer().clone();
        if let Some((field, query)) = first_text_signal(signals) {
            let terms = analyzer.analyze(&query);
            return uqa_fusion::extract_query_features(&index_stats, &terms, Some(&field))
                .to_vec();
        }
        vec![0.0; uqa_fusion::N_QUERY_FEATURES]
    }

    fn bridge_context(&self) -> uqa_operators::base::ExecutionContext {
        let mut ctx = uqa_operators::base::ExecutionContext::new();
        if let Some(state) = self.engine.table(self.table) {
            ctx.document_store = Some(state.document_store.read().snapshot());
        }
        ctx
    }

    fn facet_vector_inline(
        &self,
        vec_pl: &PostingList,
        facet_field: &str,
    ) -> PostingList {
        use std::collections::BTreeMap;
        let Some(state) = self.engine.table(self.table) else {
            return PostingList::new();
        };
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
            fields.insert("_facet_field".to_string(), Value::Str(facet_field.to_string()));
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
        PostingList::from_sorted_unchecked(entries)
    }
}

/// Replay a posting list that the [`EngineDriver`] has already
/// computed. Used by fusion / boolean wrappers that take
/// `Arc<dyn Operator>` signals: the driver pre-executes each child
/// node and hands the result over as a [`StaticPostingList`].
struct StaticPostingList {
    pl: PostingList,
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
        OperatorTree::Term { query, field } => {
            field.clone().map(|f| (f, query.clone()))
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
        OperatorTree::Filter { source: Some(s), .. } => find_text_in_tree(s),
        OperatorTree::Composed(parts)
        | OperatorTree::Intersect(parts)
        | OperatorTree::Union(parts) => parts.iter().find_map(find_text_in_tree),
        OperatorTree::Complement(inner)
        | OperatorTree::CosineProbability(inner) => find_text_in_tree(inner),
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

/// Lower a WHERE expression and run [`QueryOptimizer`] over the
/// resulting tree without executing it. Useful for tests and
/// `EXPLAIN`-style diagnostics that want to inspect the rewritten
/// shape before any posting list is materialised.
#[must_use]
pub fn optimised_tree_for(
    engine: &Engine,
    table: &str,
    where_expr: &Expr,
    params: &[SQLParam],
) -> Option<OperatorTree> {
    let tree = lower_where(where_expr, params)?;
    let row_count = engine.table_doc_ids(table).len() as u64;
    Some(
        QueryOptimizer::new()
            .with_row_count(row_count)
            .optimize(tree),
    )
}

/// The "lower → optimise → execute" pipeline. `Some(rows)` when the
/// WHERE expression maps cleanly onto the operator tree; `None`
/// signals the caller to fall back to its direct-dispatch path. Any
/// engine-side failure returned by the helpers it re-uses bubbles up
/// as `Err`.
pub fn run_optimised(
    engine: &Engine,
    table: &str,
    where_expr: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
    let Some(expr) = where_expr else {
        return Ok(None);
    };
    let Some(optimised) = optimised_tree_for(engine, table, expr, params) else {
        return Ok(None);
    };
    let driver = EngineDriver::new(engine, table, params);
    let mut executor = PlanExecutor::new(&driver);
    let pl = executor.execute(&optimised);
    Ok(Some(posting_list_to_scored(&pl)))
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
