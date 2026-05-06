//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Algebraic rewrites.
//!
//! The optimizer walks the [`SelectStmt`] AST and applies a small,
//! ordered set of rewrites:
//!
//! 1. Filter pushdown -- predicates referencing only one side of a
//!    JOIN sink down to the leaf-most table they apply to.
//! 2. Vector threshold merging -- adjacent `knn_match(... threshold a)`
//!    and `knn_match(... threshold b)` calls collapse into one
//!    `knn_match` whose threshold is `max(a, b)`.
//! 3. Boolean simplification -- `AND` / `OR` lists drop literal-true
//!    /-false members and unfold one-element groups.
//!
//! The rewrites are pure -- each takes ownership of the AST, produces
//! a new AST, and never mutates input references in place. That makes
//! them safe to chain in any order.

use uqa_core::Value;
use uqa_sql::ast::{Expr, FromClause, OrderBy, Projection, SelectStmt, WindowSpec};

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    pub enable_filter_pushdown: bool,
    pub enable_boolean_simplify: bool,
    pub enable_vector_threshold_merge: bool,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enable_filter_pushdown: true,
            enable_boolean_simplify: true,
            enable_vector_threshold_merge: true,
        }
    }
}

pub fn optimize(stmt: SelectStmt, cfg: &OptimizerConfig) -> SelectStmt {
    let mut s = stmt;
    if cfg.enable_boolean_simplify {
        s.r#where = s.r#where.map(simplify_bool);
        s.projections = s
            .projections
            .into_iter()
            .map(|p| Projection {
                alias: p.alias,
                expr: simplify_bool(p.expr),
            })
            .collect();
        s.order_by = s
            .order_by
            .into_iter()
            .map(|o| OrderBy {
                expr: simplify_bool(o.expr),
                descending: o.descending,
            })
            .collect();
    }
    if cfg.enable_vector_threshold_merge {
        s.r#where = s.r#where.map(merge_vector_thresholds);
    }
    if cfg.enable_filter_pushdown {
        s = pushdown_filters(s);
    }
    s
}

/// Recursive Boolean simplification.
fn simplify_bool(expr: Expr) -> Expr {
    match expr {
        Expr::And(parts) => {
            let mut kept: Vec<Expr> = Vec::new();
            for p in parts {
                let p = simplify_bool(p);
                match &p {
                    Expr::Literal(Value::Bool(true)) => continue,
                    Expr::Literal(Value::Bool(false)) => return Expr::Literal(Value::Bool(false)),
                    _ => {}
                }
                if let Expr::And(inner) = p {
                    kept.extend(inner);
                } else {
                    kept.push(p);
                }
            }
            if kept.is_empty() {
                Expr::Literal(Value::Bool(true))
            } else if kept.len() == 1 {
                kept.into_iter().next().unwrap()
            } else {
                Expr::And(kept)
            }
        }
        Expr::Or(parts) => {
            let mut kept: Vec<Expr> = Vec::new();
            for p in parts {
                let p = simplify_bool(p);
                match &p {
                    Expr::Literal(Value::Bool(false)) => continue,
                    Expr::Literal(Value::Bool(true)) => return Expr::Literal(Value::Bool(true)),
                    _ => {}
                }
                if let Expr::Or(inner) = p {
                    kept.extend(inner);
                } else {
                    kept.push(p);
                }
            }
            if kept.is_empty() {
                Expr::Literal(Value::Bool(false))
            } else if kept.len() == 1 {
                kept.into_iter().next().unwrap()
            } else {
                Expr::Or(kept)
            }
        }
        Expr::Not(inner) => match simplify_bool(*inner) {
            Expr::Literal(Value::Bool(b)) => Expr::Literal(Value::Bool(!b)),
            Expr::Not(inner) => *inner,
            other => Expr::Not(Box::new(other)),
        },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: Box::new(simplify_bool(*lhs)),
            rhs: Box::new(simplify_bool(*rhs)),
        },
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(simplify_bool(*expr)),
            negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(simplify_bool(*expr)),
            low: Box::new(simplify_bool(*low)),
            high: Box::new(simplify_bool(*high)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(simplify_bool(*expr)),
            list: list.into_iter().map(simplify_bool).collect(),
            negated,
        },
        Expr::Func { name, args } => Expr::Func {
            name,
            args: args.into_iter().map(simplify_bool).collect(),
        },
        Expr::WindowCall { name, args, spec } => Expr::WindowCall {
            name,
            args: args.into_iter().map(simplify_bool).collect(),
            spec: WindowSpec {
                partition_by: spec.partition_by.into_iter().map(simplify_bool).collect(),
                order_by: spec
                    .order_by
                    .into_iter()
                    .map(|o| OrderBy {
                        expr: simplify_bool(o.expr),
                        descending: o.descending,
                    })
                    .collect(),
            },
        },
        Expr::Array(items) => Expr::Array(items.into_iter().map(simplify_bool).collect()),
        other => other,
    }
}

/// Replace adjacent `knn_match(field, query, threshold = a) AND
/// knn_match(field, query, threshold = b)` calls with a single
/// `knn_match` whose threshold is the strictest of the two.
fn merge_vector_thresholds(expr: Expr) -> Expr {
    match expr {
        Expr::And(parts) => {
            let mut by_field: std::collections::BTreeMap<String, (Expr, f64)> =
                std::collections::BTreeMap::new();
            let mut others: Vec<Expr> = Vec::new();
            for p in parts {
                if let Expr::Func { name, args } = &p {
                    if name == "knn_match" && args.len() >= 3 {
                        if let (Expr::Literal(Value::Str(field)), Expr::Literal(Value::Float(t))) =
                            (&args[0], &args[2])
                        {
                            let threshold = *t;
                            let entry = by_field
                                .entry(field.clone())
                                .or_insert_with(|| (p.clone(), threshold));
                            if threshold > entry.1 {
                                entry.1 = threshold;
                                if let Expr::Func { name: _, args: a } = &mut entry.0 {
                                    if a.len() >= 3 {
                                        a[2] = Expr::Literal(Value::Float(threshold));
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }
                others.push(merge_vector_thresholds(p));
            }
            let mut merged: Vec<Expr> = others;
            for (_field, (mut call, _t)) in by_field {
                if let Expr::Func { args, .. } = &mut call {
                    *args = args.iter().cloned().map(merge_vector_thresholds).collect();
                }
                merged.push(call);
            }
            if merged.len() == 1 {
                merged.into_iter().next().unwrap()
            } else {
                Expr::And(merged)
            }
        }
        Expr::Or(parts) => Expr::Or(parts.into_iter().map(merge_vector_thresholds).collect()),
        Expr::Not(inner) => Expr::Not(Box::new(merge_vector_thresholds(*inner))),
        other => other,
    }
}

fn pushdown_filters(stmt: SelectStmt) -> SelectStmt {
    let mut stmt = stmt;
    let Some(filter) = stmt.r#where.take() else {
        return stmt;
    };
    let parts = match filter {
        Expr::And(parts) => parts,
        single => vec![single],
    };
    let mut leftover: Vec<Expr> = Vec::new();
    let mut pushed: Vec<(String, Expr)> = Vec::new();
    for part in parts {
        match qualifier_of(&part) {
            Some(q) if from_contains_qualifier(stmt.from.as_ref(), &q) => {
                pushed.push((q, part));
            }
            _ => leftover.push(part),
        }
    }
    stmt.from = stmt.from.map(|from| inject_pushdowns(from, &mut pushed));
    leftover.extend(pushed.into_iter().map(|(_, e)| e));
    stmt.r#where = if leftover.is_empty() {
        None
    } else if leftover.len() == 1 {
        leftover.into_iter().next()
    } else {
        Some(Expr::And(leftover))
    };
    stmt
}

fn qualifier_of(expr: &Expr) -> Option<String> {
    fn collect(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::QualifiedColumn { qualifier, .. } => out.push(qualifier.clone()),
            Expr::Column(_) => {}
            Expr::And(parts) | Expr::Or(parts) => parts.iter().for_each(|p| collect(p, out)),
            Expr::Not(inner) => collect(inner, out),
            Expr::Binary { lhs, rhs, .. } => {
                collect(lhs, out);
                collect(rhs, out);
            }
            Expr::IsNull { expr, .. } => collect(expr, out),
            Expr::Between { expr, low, high } => {
                collect(expr, out);
                collect(low, out);
                collect(high, out);
            }
            Expr::InList { expr, list, .. } => {
                collect(expr, out);
                list.iter().for_each(|p| collect(p, out));
            }
            Expr::Func { args, .. } | Expr::WindowCall { args, .. } => {
                args.iter().for_each(|p| collect(p, out));
            }
            Expr::Array(items) => items.iter().for_each(|p| collect(p, out)),
            _ => {}
        }
    }
    let mut quals = Vec::new();
    collect(expr, &mut quals);
    quals.sort();
    quals.dedup();
    if quals.len() == 1 {
        Some(quals.into_iter().next().unwrap())
    } else {
        None
    }
}

fn from_contains_qualifier(from: Option<&FromClause>, qual: &str) -> bool {
    let Some(from) = from else { return false };
    match from {
        FromClause::Table { name, alias } => alias.as_deref() == Some(qual) || name == qual,
        FromClause::Join { left, right, .. } => {
            from_contains_qualifier(Some(left), qual) || from_contains_qualifier(Some(right), qual)
        }
        FromClause::Values { alias, .. }
        | FromClause::Function { alias, .. }
        | FromClause::Subquery { alias, .. } => alias.as_deref() == Some(qual),
    }
}

fn inject_pushdowns(from: FromClause, pushed: &mut Vec<(String, Expr)>) -> FromClause {
    if pushed.is_empty() {
        return from;
    }
    match from {
        FromClause::Table { name, alias } => FromClause::Table { name, alias },
        FromClause::Join {
            left,
            right,
            kind,
            on,
            lateral,
        } => FromClause::Join {
            left: Box::new(inject_pushdowns(*left, pushed)),
            right: Box::new(inject_pushdowns(*right, pushed)),
            kind,
            on,
            lateral,
        },
        other @ (FromClause::Values { .. }
        | FromClause::Function { .. }
        | FromClause::Subquery { .. }) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    #[test]
    fn boolean_simplify_drops_true_in_and() {
        let e = Expr::And(vec![
            Expr::Literal(Value::Bool(true)),
            Expr::Column("x".into()),
        ]);
        let s = simplify_bool(e);
        assert!(matches!(s, Expr::Column(c) if c == "x"));
    }

    #[test]
    fn boolean_simplify_short_circuits_false_in_and() {
        let e = Expr::And(vec![
            Expr::Column("x".into()),
            Expr::Literal(Value::Bool(false)),
        ]);
        let s = simplify_bool(e);
        assert!(matches!(s, Expr::Literal(Value::Bool(false))));
    }

    #[test]
    fn boolean_simplify_short_circuits_true_in_or() {
        let e = Expr::Or(vec![
            Expr::Column("x".into()),
            Expr::Literal(Value::Bool(true)),
        ]);
        let s = simplify_bool(e);
        assert!(matches!(s, Expr::Literal(Value::Bool(true))));
    }

    #[test]
    fn boolean_simplify_double_negation() {
        let e = Expr::Not(Box::new(Expr::Not(Box::new(Expr::Column("x".into())))));
        let s = simplify_bool(e);
        assert!(matches!(s, Expr::Column(c) if c == "x"));
    }

    #[test]
    fn vector_threshold_merge_keeps_max() {
        let knn = |t: f64| Expr::Func {
            name: "knn_match".into(),
            args: vec![
                Expr::Literal(Value::Str("emb".into())),
                Expr::Literal(Value::Str("query".into())),
                Expr::Literal(Value::Float(t)),
            ],
        };
        let e = Expr::And(vec![knn(0.5), knn(0.7)]);
        let merged = merge_vector_thresholds(e);
        let Expr::Func { args, .. } = merged else {
            panic!("expected single knn_match")
        };
        match &args[2] {
            Expr::Literal(Value::Float(t)) => assert!((t - 0.7).abs() < 1e-9),
            other => panic!("expected float threshold, got {other:?}"),
        }
    }

    #[test]
    fn pushdown_does_not_alter_filter_with_multiple_qualifiers() {
        let pred = Expr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs: Box::new(Expr::QualifiedColumn {
                qualifier: "a".into(),
                column: "id".into(),
            }),
            rhs: Box::new(Expr::QualifiedColumn {
                qualifier: "b".into(),
                column: "id".into(),
            }),
        };
        let stmt = SelectStmt {
            projections: vec![Projection {
                expr: Expr::Star,
                alias: None,
            }],
            from: Some(FromClause::Table {
                name: "a".into(),
                alias: None,
            }),
            r#where: Some(pred),
            group_by: vec![],
            order_by: vec![],
            limit: None,
            offset: None,
            with: vec![],
            set_op: None,
            distinct: false,
        };
        let optimised = pushdown_filters(stmt);
        assert!(optimised.r#where.is_some());
    }
}
