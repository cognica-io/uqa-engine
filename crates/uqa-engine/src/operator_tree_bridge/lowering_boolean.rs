//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boolean and comparison lowering for SQL predicates.

use super::{
    const_f64, const_value, lower_bayesian_evidence_fusion, lower_bayesian_match_with_prior,
    lower_calibrated_vector_match, lower_graph_function, lower_learned_fusion,
    lower_multi_field_match, lower_operator_arg, lower_positive_evidence_pool,
    lower_staged_retrieval, try_lower_attention_fusion, try_lower_fts_match, try_lower_knn_match,
    try_lower_text_match, BinaryOp, OperatorTree, Predicate, SQLParam, ScalarExpr, TextScoringMode,
};

/// Build document-level SQL Boolean algebra without weakening the carrier
/// contract of the generic set operators. Homogeneous graph children keep
/// their `GraphPostingList` carrier and its explicit subgraph merge policy.
/// Only a heterogeneous SQL predicate inserts the lossless Phi codec at each
/// graph/document boundary.
pub(super) fn lower_document_boolean(mut children: Vec<OperatorTree>, union: bool) -> OperatorTree {
    let graph_children = children
        .iter()
        .filter(|child| tree_returns_graph(child))
        .count();
    if graph_children > 0 && graph_children < children.len() {
        children = children
            .into_iter()
            .map(|child| {
                if tree_returns_graph(&child) {
                    OperatorTree::EncodeGraphPosting {
                        source: Box::new(child),
                    }
                } else {
                    child
                }
            })
            .collect();
    }
    if union {
        OperatorTree::Union(children)
    } else {
        OperatorTree::Intersect(children)
    }
}

pub(super) fn tree_returns_graph(tree: &OperatorTree) -> bool {
    match tree {
        OperatorTree::Traverse { .. }
        | OperatorTree::PatternMatch { .. }
        | OperatorTree::RegularPathQuery { .. }
        | OperatorTree::WeightedPathQuery { .. }
        | OperatorTree::MessagePassing { .. }
        | OperatorTree::GraphEmbedding { .. }
        | OperatorTree::PageRank { .. }
        | OperatorTree::HITS { .. }
        | OperatorTree::BetweennessCentrality { .. }
        | OperatorTree::TemporalTraverse { .. }
        | OperatorTree::TemporalPatternMatch { .. } => true,
        OperatorTree::Intersect(children) | OperatorTree::Union(children) => {
            !children.is_empty() && children.iter().all(tree_returns_graph)
        }
        OperatorTree::Composed(children) => children.last().is_some_and(tree_returns_graph),
        _ => false,
    }
}

pub(super) fn lower_function(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "text_match" => {
            try_lower_text_match("text_match", args, params, TextScoringMode::BM25).ok()
        }
        "bayesian_match" => try_lower_text_match(
            "bayesian_match",
            args,
            params,
            TextScoringMode::BayesianBM25,
        )
        .ok(),
        "fts_match" => try_lower_fts_match(args, params).ok(),
        "bayesian_match_with_prior" => lower_bayesian_match_with_prior(args, params),
        "calibrated_vector_match" => lower_calibrated_vector_match(args, params),
        // Standalone knn_match preserves raw cosine similarities;
        // calibration to (0, 1) only fires inside fusion contexts.
        "knn_match" => try_lower_knn_match(args, params).ok(),
        "fuse_bayesian_evidence" | "fuse_log_odds" => lower_bayesian_evidence_fusion(args, params),
        "pool_positive_evidence" => lower_positive_evidence_pool(args, params),
        "multi_field_match" => lower_multi_field_match(args, params),
        "staged_retrieval" => lower_staged_retrieval(args, params),
        "attention" | "fuse_attention" | "fuse_multihead" => {
            try_lower_attention_fusion(&lower, args, params).ok()
        }
        "learned_fusion" | "fuse_learned" => lower_learned_fusion(args, params),
        "sparse_threshold" => {
            if args.len() != 2 {
                return None;
            }
            let source = lower_operator_arg(args.first()?, params)?;
            let threshold = const_f64(args.get(1)?, params)?;
            Some(OperatorTree::SparseThreshold {
                source: Box::new(source),
                threshold,
            })
        }
        _ => lower_graph_function(&lower, args, params),
    }
}

pub(super) fn lower_comparison(
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

pub(super) fn column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}
