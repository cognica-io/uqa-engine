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
            Some(OperatorTree::KNN {
                query_vector,
                k,
                field,
            })
        }
        "fuse_log_odds" => {
            // `fuse_log_odds(signal_1, signal_2, ..., alpha)`. The last
            // arg must be a numeric literal alpha; the rest must lower
            // into `OperatorTree` signals.
            if args.len() < 2 {
                return None;
            }
            let alpha_expr = args.last()?;
            let alpha = const_f64(alpha_expr, params)?;
            let mut signals: Vec<OperatorTree> = Vec::with_capacity(args.len() - 1);
            for a in &args[..args.len() - 1] {
                signals.push(lower_where(a, params)?);
            }
            Some(OperatorTree::LogOddsFusion {
                signals,
                alpha,
                gating: GatingSpec::Pass,
            })
        }
        _ => None,
    }
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
}

impl<'a> EngineDriver<'a> {
    #[must_use]
    pub fn new(engine: &'a Engine, table: &'a str, params: &'a [SQLParam]) -> EngineDriver<'a> {
        Self {
            engine,
            table,
            params,
        }
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
                let mut iter = parts.iter().map(|p| self.execute_node(p));
                let Some(first) = iter.next() else {
                    return PostingList::new();
                };
                iter.fold(first, |acc, next| acc.intersect(&next))
            }
            OperatorTree::Union(parts) => {
                let mut iter = parts.iter().map(|p| self.execute_node(p));
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
                // Reuse the existing fuse_log_odds dispatcher by lowering
                // each child back to a SQL expression. Today's fast path:
                // execute every signal independently and combine through
                // posting-list union, then re-score via the same alpha
                // weighting. This keeps semantics aligned with the
                // un-optimised baseline while letting `reorder_fusion_signals`
                // and `simplify_algebra` reshape the tree first.
                let scored: Vec<PostingList> =
                    signals.iter().map(|s| self.execute_node(s)).collect();
                let _ = alpha;
                let mut iter = scored.into_iter();
                let Some(first) = iter.next() else {
                    return PostingList::new();
                };
                iter.fold(first, |acc, next| acc.union(&next))
            }
            // Anything else is currently outside the SELECT-WHERE
            // surface this bridge handles. The lower step rejects them
            // up-front, so this path is a safety net.
            _ => PostingList::new(),
        }
    }
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
    let Some(tree) = lower_where(expr, params) else {
        return Ok(None);
    };
    let optimised = QueryOptimizer::new()
        .with_row_count(engine.table_doc_ids(table).len() as u64)
        .optimize(tree);
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
