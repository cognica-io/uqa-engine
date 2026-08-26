//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic fusion for mixed text and vector retrieval predicates.

use super::{QueryPlan, RelationalPlan, ScalarExpr, SourcePlan};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetrievalSignalKind {
    Text,
    Vector,
}

struct RetrievalSignal {
    index: usize,
    kind: RetrievalSignalKind,
    qualifier: Option<String>,
}

/// Rewrite a conjunction containing both text and vector retrieval leaves into
/// canonical exact Bayesian evidence fusion. Explicit fusion functions are
/// never rewritten, and relational conjuncts remain strict filters over the
/// fused candidate support.
pub(super) fn rewrite_implicit_hybrid_fusion(
    expression: &mut ScalarExpr,
    allow_unqualified_signals: bool,
) {
    match expression {
        ScalarExpr::And(parts) => {
            let mut flattened = flatten_conjunction(std::mem::take(parts));
            for part in &mut flattened {
                rewrite_implicit_hybrid_fusion(part, allow_unqualified_signals);
            }
            let rewritten = rewrite_conjunction(flattened, allow_unqualified_signals);
            *expression = rewritten;
        }
        ScalarExpr::Or(parts) => {
            for part in parts {
                rewrite_implicit_hybrid_fusion(part, allow_unqualified_signals);
            }
        }
        _ => {}
    }
}

/// Return whether optimizing this query will introduce a fusion node that can
/// persist Bayesian text calibration. This mirrors the rewrite's source-aware
/// qualifier policy before optimization changes the predicate.
pub fn query_contains_implicit_hybrid_fusion(query: &QueryPlan) -> bool {
    query
        .ctes
        .iter()
        .any(|cte| query_contains_implicit_hybrid_fusion(&cte.query))
        || relational_contains_implicit_hybrid_fusion(&query.root)
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
            let mut flattened = Vec::new();
            flatten_conjunction_refs(parts, &mut flattened);
            can_implicitly_fuse(&flattened, allow_unqualified_signals)
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

fn flatten_conjunction(parts: Vec<ScalarExpr>) -> Vec<ScalarExpr> {
    let mut flattened = Vec::new();
    for part in parts {
        if let ScalarExpr::And(children) = part {
            flattened.extend(flatten_conjunction(children));
        } else {
            flattened.push(part);
        }
    }
    flattened
}

fn flatten_conjunction_refs<'a>(parts: &'a [ScalarExpr], output: &mut Vec<&'a ScalarExpr>) {
    for part in parts {
        if let ScalarExpr::And(children) = part {
            flatten_conjunction_refs(children, output);
        } else {
            output.push(part);
        }
    }
}

fn rewrite_conjunction(parts: Vec<ScalarExpr>, allow_unqualified_signals: bool) -> ScalarExpr {
    let Some(signals) = implicit_fusion_signals(&parts, allow_unqualified_signals) else {
        return ScalarExpr::And(parts);
    };
    let signal_indexes: std::collections::BTreeSet<usize> =
        signals.iter().map(|signal| signal.index).collect();
    let mut fusion_signals = Vec::with_capacity(signals.len());
    let mut residual = Vec::with_capacity(parts.len() - signals.len());
    for (index, expression) in parts.into_iter().enumerate() {
        if signal_indexes.contains(&index) {
            fusion_signals.push(calibrate_text_signal(expression));
        } else {
            residual.push(expression);
        }
    }

    let fusion = ScalarExpr::Func {
        name: "fuse_bayesian_evidence".to_string(),
        binding: None,
        args: fusion_signals,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    if residual.is_empty() {
        fusion
    } else {
        let mut conjuncts = Vec::with_capacity(residual.len() + 1);
        conjuncts.push(fusion);
        conjuncts.extend(residual);
        ScalarExpr::And(conjuncts)
    }
}

fn implicit_fusion_signals(
    parts: &[ScalarExpr],
    allow_unqualified_signals: bool,
) -> Option<Vec<RetrievalSignal>> {
    if parts.iter().any(contains_explicit_fusion) {
        return None;
    }
    let signals: Vec<RetrievalSignal> = parts
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let (kind, qualifier) = classify_signal(expression)?;
            Some(RetrievalSignal {
                index,
                kind,
                qualifier: qualifier.map(str::to_string),
            })
        })
        .collect();
    let has_text = signals
        .iter()
        .any(|signal| signal.kind == RetrievalSignalKind::Text);
    let has_vector = signals
        .iter()
        .any(|signal| signal.kind == RetrievalSignalKind::Vector);
    if !has_text || !has_vector || !signals_share_qualifier(&signals, allow_unqualified_signals) {
        return None;
    }
    Some(signals)
}

fn can_implicitly_fuse(parts: &[&ScalarExpr], allow_unqualified_signals: bool) -> bool {
    if parts.iter().any(|part| contains_explicit_fusion(part)) {
        return false;
    }
    let signals: Vec<(RetrievalSignalKind, Option<&str>)> = parts
        .iter()
        .filter_map(|expression| classify_signal(expression))
        .collect();
    let has_text = signals
        .iter()
        .any(|(kind, _)| *kind == RetrievalSignalKind::Text);
    let has_vector = signals
        .iter()
        .any(|(kind, _)| *kind == RetrievalSignalKind::Vector);
    let qualifiers_share_source = signals.first().is_some_and(|(_, first)| match first {
        Some(qualifier) => signals
            .iter()
            .all(|(_, candidate)| *candidate == Some(*qualifier)),
        None => {
            allow_unqualified_signals && signals.iter().all(|(_, candidate)| candidate.is_none())
        }
    });
    has_text && has_vector && qualifiers_share_source
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

    let kind = match name.to_ascii_lowercase().as_str() {
        "text_match" | "bayesian_match" => RetrievalSignalKind::Text,
        "knn_match" | "calibrated_vector_match" => RetrievalSignalKind::Vector,
        _ => return None,
    };
    let qualifier = match args.first()? {
        ScalarExpr::Column(_) | ScalarExpr::Literal(_) => None,
        ScalarExpr::QualifiedColumn { qualifier, .. } => Some(qualifier.as_str()),
        _ => return None,
    };
    Some((kind, qualifier))
}

fn signals_share_qualifier(signals: &[RetrievalSignal], allow_unqualified_signals: bool) -> bool {
    let Some(first) = signals.first() else {
        return false;
    };
    match first.qualifier.as_deref() {
        Some(qualifier) => signals
            .iter()
            .all(|signal| signal.qualifier.as_deref() == Some(qualifier)),
        None => {
            allow_unqualified_signals && signals.iter().all(|signal| signal.qualifier.is_none())
        }
    }
}

fn calibrate_text_signal(mut expression: ScalarExpr) -> ScalarExpr {
    if let ScalarExpr::Func { name, .. } = &mut expression {
        if name.eq_ignore_ascii_case("text_match") {
            *name = "bayesian_match".to_string();
        }
    }
    expression
}

fn is_explicit_fusion_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "pool_positive_evidence"
            | "fuse_log_odds"
            | "fuse_bayesian_evidence"
            | "attention"
            | "fuse_attention"
            | "fuse_multihead"
            | "learned_fusion"
            | "fuse_learned"
    )
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
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(contains_explicit_fusion),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            contains_explicit_fusion(lhs) || contains_explicit_fusion(rhs)
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => contains_explicit_fusion(inner),
        ScalarExpr::Between { expr, low, high } => {
            contains_explicit_fusion(expr)
                || contains_explicit_fusion(low)
                || contains_explicit_fusion(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            contains_explicit_fusion(expr) || list.iter().any(contains_explicit_fusion)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(contains_explicit_fusion)
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
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}
