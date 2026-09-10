//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate constant equality predicates across equivalent join keys.

use std::collections::BTreeSet;
use uqa_sql::{
    plan::{source_projection::QualifierFilters, SourcePlan},
    ScalarExpr,
};

pub fn propagated_join_filters(
    filters: &QualifierFilters,
    source_from: &SourcePlan,
    target_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Option<QualifierFilters> {
    let on = on?;
    let mut out = filters.clone();
    let mut changed = false;
    let source_quals = from_qualifiers(source_from);
    let target_quals = from_qualifiers(target_from);
    for (left, right) in join_column_equalities(on) {
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &left,
            &right,
        );
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &right,
            &left,
        );
    }
    changed.then_some(out)
}

pub fn propagate_join_filter_pair(
    filters: &QualifierFilters,
    out: &mut QualifierFilters,
    source_quals: &BTreeSet<String>,
    target_quals: &BTreeSet<String>,
    source: &(String, String),
    target: &(String, String),
) -> bool {
    if !source_quals.contains(&source.0) || !target_quals.contains(&target.0) {
        return false;
    }
    let mut changed = false;
    if let Some(source_filters) = filters.get(&source.0) {
        for filter in source_filters {
            if let Some(value) = constant_equality_for_column(filter, &source.0, &source.1) {
                let propagated = ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Equal,
                    lhs: Box::new(ScalarExpr::qualified_column(&target.0, &target.1)),
                    rhs: Box::new(value),
                };
                out.entry(target.0.clone()).or_default().push(propagated);
                changed = true;
            }
        }
    }
    changed
}

pub fn constant_equality_for_column(
    expr: &ScalarExpr,
    qual: &str,
    column: &str,
) -> Option<ScalarExpr> {
    let ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    } = expr
    else {
        return None;
    };
    if expr_is_qualified_column(lhs, qual, column) && expr_is_constant(rhs) {
        return Some((**rhs).clone());
    }
    if expr_is_qualified_column(rhs, qual, column) && expr_is_constant(lhs) {
        return Some((**lhs).clone());
    }
    None
}

pub fn expr_is_qualified_column(expr: &ScalarExpr, qual: &str, column: &str) -> bool {
    matches!(
        expr,
        ScalarExpr::QualifiedColumn {
            qualifier,
            column: col,
            ..
        } if qualifier == qual && col == column
    )
}

pub fn expr_is_constant(expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Literal(_) | ScalarExpr::Param(_))
}

pub fn join_column_equalities(expr: &ScalarExpr) -> Vec<((String, String), (String, String))> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(join_column_equalities).collect(),
        ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } => {
            let Some(left) = qualified_column_pair(lhs) else {
                return Vec::new();
            };
            let Some(right) = qualified_column_pair(rhs) else {
                return Vec::new();
            };
            vec![(left, right)]
        }
        _ => Vec::new(),
    }
}

pub fn qualified_column_pair(expr: &ScalarExpr) -> Option<(String, String)> {
    match expr {
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => Some((qualifier.clone(), column.clone())),
        _ => None,
    }
}

pub fn from_qualifiers(from: &SourcePlan) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_from_qualifiers(from, &mut out);
    out
}

pub fn collect_from_qualifiers(from: &SourcePlan, out: &mut BTreeSet<String>) {
    match from {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                out.insert(alias.clone());
            } else {
                collect_from_qualifiers(left, out);
                collect_from_qualifiers(right, out);
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = from.visible_qualifier() {
                out.insert(qualifier.to_string());
            }
        }
    }
}
