//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic fusion for mixed text and vector retrieval predicates.

use super::{QueryPlan, RelationalPlan, ScalarExpr, SourcePlan};

pub(super) mod owned;
mod summary;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetrievalSignalKind {
    Text,
    Vector,
}

/// Rewrite a conjunction containing both text and vector retrieval leaves into
/// canonical exact Bayesian evidence fusion. Explicit fusion functions are
/// never rewritten, and relational conjuncts remain strict filters over the
/// fused candidate support.
pub(super) fn rewrite_implicit_hybrid_fusion(
    expression: &mut ScalarExpr,
    allow_unqualified_signals: bool,
) {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let input = std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null));
    *expression = owned::rewrite(
        uqa_sql::plan::OwnedScalarExpr::uncontrolled(input),
        allow_unqualified_signals,
        &control,
    )
    .expect("ordinary fusion construction")
    .finish(&control)
    .expect("ordinary fusion result")
    .into_uncontrolled()
    .expect("ordinary fusion owner");
}

/// Return whether optimizing this query will introduce a fusion node that can
/// persist Bayesian text calibration. This mirrors the rewrite's source-aware
/// qualifier policy before optimization changes the predicate.
pub fn query_contains_implicit_hybrid_fusion(query: &QueryPlan) -> bool {
    query.ctes.iter().any(cte_contains_implicit_hybrid_fusion)
        || relational_contains_implicit_hybrid_fusion(&query.root)
}

fn cte_contains_implicit_hybrid_fusion(cte: &crate::CtePlan) -> bool {
    match &cte.body {
        crate::CtePlanBody::Query(query) => query_contains_implicit_hybrid_fusion(query),
        crate::CtePlanBody::Command(command) => match command.as_ref() {
            crate::CommandPlan::Insert(plan) => {
                plan.ctes.iter().any(cte_contains_implicit_hybrid_fusion)
                    || plan
                        .source
                        .as_deref()
                        .is_some_and(query_contains_implicit_hybrid_fusion)
                    || plan
                        .subqueries
                        .iter()
                        .any(query_contains_implicit_hybrid_fusion)
            }
            crate::CommandPlan::Update(plan) => {
                plan.ctes.iter().any(cte_contains_implicit_hybrid_fusion)
                    || plan
                        .source
                        .as_deref()
                        .is_some_and(source_contains_implicit_hybrid_fusion)
                    || plan
                        .subqueries
                        .iter()
                        .any(query_contains_implicit_hybrid_fusion)
            }
            crate::CommandPlan::Delete(plan) => {
                plan.ctes.iter().any(cte_contains_implicit_hybrid_fusion)
                    || plan
                        .source
                        .as_deref()
                        .is_some_and(source_contains_implicit_hybrid_fusion)
                    || plan
                        .subqueries
                        .iter()
                        .any(query_contains_implicit_hybrid_fusion)
            }
            crate::CommandPlan::Merge(plan) => {
                plan.ctes.iter().any(cte_contains_implicit_hybrid_fusion)
                    || source_contains_implicit_hybrid_fusion(&plan.source)
                    || plan
                        .subqueries
                        .iter()
                        .any(query_contains_implicit_hybrid_fusion)
            }
            _ => false,
        },
    }
}

fn relational_contains_implicit_hybrid_fusion(plan: &RelationalPlan) -> bool {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            let allow_unqualified_signals = source_allows_unqualified_signals(block.from.as_ref());
            block.r#where.as_ref().is_some_and(|expression| {
                contains_implicit_hybrid_fusion(expression, allow_unqualified_signals)
            }) || block
                .from
                .as_ref()
                .is_some_and(source_contains_implicit_hybrid_fusion)
                || block
                    .subqueries
                    .iter()
                    .any(query_contains_implicit_hybrid_fusion)
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            query_contains_implicit_hybrid_fusion(left)
                || query_contains_implicit_hybrid_fusion(right)
                || subqueries.iter().any(query_contains_implicit_hybrid_fusion)
        }
        RelationalPlan::Values { subqueries, .. } => {
            subqueries.iter().any(query_contains_implicit_hybrid_fusion)
        }
    }
}

fn source_contains_implicit_hybrid_fusion(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::RetainedRows(_) => false,
        SourcePlan::Join { left, right, .. } => {
            source_contains_implicit_hybrid_fusion(left)
                || source_contains_implicit_hybrid_fusion(right)
        }
        SourcePlan::Subquery { body, .. } => query_contains_implicit_hybrid_fusion(body),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => false,
    }
}

pub(super) fn source_allows_unqualified_signals(source: Option<&SourcePlan>) -> bool {
    source.is_some_and(|source| !matches!(source, SourcePlan::Join { .. }))
}

fn contains_implicit_hybrid_fusion(
    expression: &ScalarExpr,
    allow_unqualified_signals: bool,
) -> bool {
    match expression {
        ScalarExpr::And(parts) => {
            let mut signals = summary::Signals::default();
            signals.observe_conjunction(expression);
            signals.can_fuse(allow_unqualified_signals)
                || parts
                    .iter()
                    .any(|part| contains_implicit_hybrid_fusion(part, allow_unqualified_signals))
        }
        ScalarExpr::Or(parts) => parts
            .iter()
            .any(|part| contains_implicit_hybrid_fusion(part, allow_unqualified_signals)),
        _ => false,
    }
}

fn classify_signal(expression: &ScalarExpr) -> Option<(RetrievalSignalKind, Option<&str>)> {
    let ScalarExpr::Func {
        name,
        args,
        distinct,
        order_by,
        filter,
        ..
    } = expression
    else {
        return None;
    };
    if *distinct || !order_by.is_empty() || filter.is_some() {
        return None;
    }

    let kind =
        if name.eq_ignore_ascii_case("text_match") || name.eq_ignore_ascii_case("bayesian_match") {
            RetrievalSignalKind::Text
        } else if name.eq_ignore_ascii_case("knn_match")
            || name.eq_ignore_ascii_case("calibrated_vector_match")
        {
            RetrievalSignalKind::Vector
        } else {
            return None;
        };
    let qualifier = match args.first()? {
        ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_) => None,
        ScalarExpr::QualifiedColumn { qualifier, .. } => Some(qualifier.as_str()),
        _ => return None,
    };
    Some((kind, qualifier))
}

fn is_explicit_fusion_name(name: &str) -> bool {
    [
        "pool_positive_evidence",
        "fuse_log_odds",
        "fuse_bayesian_evidence",
        "attention",
        "fuse_attention",
        "fuse_multihead",
        "learned_fusion",
        "fuse_learned",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn contains_explicit_fusion(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            is_explicit_fusion_name(name)
                || args.iter().any(contains_explicit_fusion)
                || order_by
                    .iter()
                    .any(|order| contains_explicit_fusion(&order.expr))
                || filter.as_deref().is_some_and(contains_explicit_fusion)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(contains_explicit_fusion),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            contains_explicit_fusion(lhs) || contains_explicit_fusion(rhs)
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => contains_explicit_fusion(inner),
        ScalarExpr::Between { expr, low, high } => {
            contains_explicit_fusion(expr)
                || contains_explicit_fusion(low)
                || contains_explicit_fusion(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            contains_explicit_fusion(expr) || list.iter().any(contains_explicit_fusion)
        }
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => {
            args.iter().any(contains_explicit_fusion)
                || filter.as_deref().is_some_and(contains_explicit_fusion)
                || spec.partition_by.iter().any(contains_explicit_fusion)
                || spec
                    .order_by
                    .iter()
                    .any(|order| contains_explicit_fusion(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(contains_explicit_fusion)
                || when.iter().any(|(condition, result)| {
                    contains_explicit_fusion(condition) || contains_explicit_fusion(result)
                })
                || else_branch.as_deref().is_some_and(contains_explicit_fusion)
        }
        ScalarExpr::InSubquery { expr, .. } => contains_explicit_fusion(expr),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}
