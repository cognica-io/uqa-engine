//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload-safe algebraic rewrites for operator trees.

use uqa_operators::OperatorTree;

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // 1. Algebraic simplification
    // ---------------------------------------------------------------

    pub(super) fn simplify_algebra(&self, op: OperatorTree) -> OperatorTree {
        // Recurse first (bottom-up).
        let op = self.recurse_simplify(op);
        match op {
            OperatorTree::Intersect(operands) => {
                // Empty elimination: any empty child collapses the
                // intersection.
                for child in &operands {
                    if child.is_empty() {
                        return OperatorTree::Intersect(Vec::new());
                    }
                }
                // Idempotence is valid only for membership-only operands.
                // Posting-list merges add scores, so structurally equal scored
                // terms must remain distinct.
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in operands {
                    for s in &seen {
                        if same_membership_term(s, &child) {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Union(A, ...) when A also appears
                // in the intersection.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for (child_index, child) in operands.iter().enumerate() {
                    if let OperatorTree::Union(union_operands) = child {
                        let drop = is_membership_only(child)
                            && operands.iter().enumerate().any(|(other_index, other)| {
                                other_index != child_index
                                    && union_operands
                                        .iter()
                                        .any(|union_term| same_membership_term(union_term, other))
                            });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    if let Some(only) = absorbed.pop() {
                        return only;
                    }
                }
                OperatorTree::Intersect(absorbed)
            }
            OperatorTree::Union(operands) => {
                // Drop empty children.
                let mut kept: Vec<OperatorTree> =
                    operands.into_iter().filter(|c| !c.is_empty()).collect();
                // Idempotence is valid only for membership-only operands.
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in kept.drain(..) {
                    for s in &seen {
                        if same_membership_term(s, &child) {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Intersect(A, ...) when A also appears
                // in the union.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for (child_index, child) in operands.iter().enumerate() {
                    if let OperatorTree::Intersect(int_operands) = child {
                        let drop = is_membership_only(child)
                            && operands.iter().enumerate().any(|(other_index, other)| {
                                other_index != child_index
                                    && int_operands.iter().any(|intersect_term| {
                                        same_membership_term(intersect_term, other)
                                    })
                            });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    if let Some(only) = absorbed.pop() {
                        return only;
                    }
                }
                if absorbed.is_empty() {
                    return OperatorTree::Union(Vec::new());
                }
                OperatorTree::Union(absorbed)
            }
            other => other,
        }
    }

    pub(super) fn recurse_simplify(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.simplify_algebra(child))
    }

    // ---------------------------------------------------------------
    // 7. Merge adjacent vector thresholds
    // ---------------------------------------------------------------

    pub(super) fn merge_vector_thresholds(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(operands) = op {
            let mut vector_ops: Vec<(Vec<f32>, f32, String)> = Vec::new();
            let mut other_ops: Vec<OperatorTree> = Vec::new();
            for child in operands {
                let child = self.recurse_children(child);
                match child {
                    OperatorTree::VectorSimilarity {
                        query_vector,
                        threshold,
                        field,
                    } => vector_ops.push((query_vector, threshold, field)),
                    other => other_ops.push(other),
                }
            }
            let mut merged_vectors: Vec<OperatorTree> = Vec::new();
            let mut used = vec![false; vector_ops.len()];
            for i in 0..vector_ops.len() {
                if used[i] {
                    continue;
                }
                let (q, mut t, f) = (
                    vector_ops[i].0.clone(),
                    vector_ops[i].1,
                    vector_ops[i].2.clone(),
                );
                for j in (i + 1)..vector_ops.len() {
                    if used[j] {
                        continue;
                    }
                    if vector_ops[j].2 == f && vectors_close(&q, &vector_ops[j].0) {
                        t = t.max(vector_ops[j].1);
                        used[j] = true;
                    }
                }
                used[i] = true;
                merged_vectors.push(OperatorTree::VectorSimilarity {
                    query_vector: q,
                    threshold: t,
                    field: f,
                });
            }
            let mut all = other_ops;
            all.extend(merged_vectors);
            if all.len() == 1 {
                if let Some(only) = all.pop() {
                    return only;
                }
            }
            return OperatorTree::Intersect(all);
        }
        self.recurse_children(op)
    }
}

/// Whether Boolean composition observes only membership for this subtree.
///
/// `PostingList::merge_union` and `PostingList::merge_intersection` add scores when the same
/// document appears on both sides. They may also carry operator-specific
/// fields. The algebraic identities are therefore safe only for the small,
/// explicit subset below, whose execution produces default payloads. Keeping
/// this match exhaustive makes a new `OperatorTree` variant opt out until its
/// payload effect has been reviewed.
fn is_membership_only(op: &OperatorTree) -> bool {
    match op {
        OperatorTree::Empty | OperatorTree::IndexScan { .. } => true,
        OperatorTree::Filter { source, .. } => source.as_deref().is_none_or(is_membership_only),
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().all(is_membership_only),
        OperatorTree::Complement(child) => is_membership_only(child),
        OperatorTree::VectorExclusion { positive, negative } => {
            is_membership_only(positive) && is_membership_only(negative)
        }
        OperatorTree::Term { .. }
        | OperatorTree::Facet { .. }
        | OperatorTree::Score { .. }
        | OperatorTree::BayesianScore { .. }
        | OperatorTree::EncodeGraphPosting { .. }
        | OperatorTree::BayesianMatchWithPrior { .. }
        | OperatorTree::VectorSimilarity { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::CosineProbability(_)
        | OperatorTree::BayesianEvidenceFusion { .. }
        | OperatorTree::RobustPositiveEvidencePool { .. }
        | OperatorTree::ProbBoolFusion { .. }
        | OperatorTree::ProbNot { .. }
        | OperatorTree::AttentionFusion { .. }
        | OperatorTree::LearnedFusion { .. }
        | OperatorTree::SparseThreshold { .. }
        | OperatorTree::Traverse { .. }
        | OperatorTree::GraphNeighbors { .. }
        | OperatorTree::GraphEdges { .. }
        | OperatorTree::PatternMatch { .. }
        | OperatorTree::RegularPathQuery { .. }
        | OperatorTree::GraphJoin { .. }
        | OperatorTree::Aggregate { .. }
        | OperatorTree::GroupBy { .. }
        | OperatorTree::MultiStage { .. }
        | OperatorTree::MultiFieldSearch { .. }
        | OperatorTree::HybridTextVector { .. }
        | OperatorTree::SemanticFilter { .. }
        | OperatorTree::FacetVector { .. }
        | OperatorTree::VertexAggregation { .. }
        | OperatorTree::WeightedPathQuery { .. }
        | OperatorTree::MessagePassing { .. }
        | OperatorTree::GraphEmbedding { .. }
        | OperatorTree::PageRank { .. }
        | OperatorTree::HITS { .. }
        | OperatorTree::BetweennessCentrality { .. }
        | OperatorTree::TextSimilarityJoin { .. }
        | OperatorTree::VectorSimilarityJoin { .. }
        | OperatorTree::HybridJoin { .. }
        | OperatorTree::CrossParadigmJoin { .. }
        | OperatorTree::TemporalTraverse { .. }
        | OperatorTree::TemporalPatternMatch { .. }
        | OperatorTree::ProgressiveFusion { .. }
        | OperatorTree::DeepFusion { .. }
        | OperatorTree::DeepPredict { .. }
        | OperatorTree::Opaque { .. } => false,
    }
}

/// Address-independent structural equivalence for operands on which Boolean
/// idempotence and absorption preserve the complete posting-list payload.
fn same_membership_term(left: &OperatorTree, right: &OperatorTree) -> bool {
    if !is_membership_only(left) || !is_membership_only(right) {
        return false;
    }

    // All three zero-child composition forms execute as the same empty set.
    if left.is_empty() || right.is_empty() {
        return left.is_empty() && right.is_empty();
    }

    match (left, right) {
        (
            OperatorTree::Filter {
                field: left_field,
                predicate: left_predicate,
                source: left_source,
            },
            OperatorTree::Filter {
                field: right_field,
                predicate: right_predicate,
                source: right_source,
            },
        ) => {
            left_field == right_field
                && left_predicate == right_predicate
                && same_optional_membership_source(left_source.as_deref(), right_source.as_deref())
        }
        (
            OperatorTree::IndexScan {
                index_name: left_index,
                field: left_field,
                predicate: left_predicate,
            },
            OperatorTree::IndexScan {
                index_name: right_index,
                field: right_field,
                predicate: right_predicate,
            },
        ) => {
            left_index == right_index
                && left_field == right_field
                && left_predicate == right_predicate
        }
        (OperatorTree::Intersect(left), OperatorTree::Intersect(right))
        | (OperatorTree::Union(left), OperatorTree::Union(right)) => {
            same_membership_multiset(left, right)
        }
        (OperatorTree::Complement(left), OperatorTree::Complement(right)) => {
            same_membership_term(left, right)
        }
        (OperatorTree::Composed(left), OperatorTree::Composed(right)) => {
            same_membership_sequence(left, right)
        }
        (
            OperatorTree::VectorExclusion {
                positive: left_positive,
                negative: left_negative,
            },
            OperatorTree::VectorExclusion {
                positive: right_positive,
                negative: right_negative,
            },
        ) => {
            same_membership_term(left_positive, right_positive)
                && same_membership_term(left_negative, right_negative)
        }
        _ => false,
    }
}

fn same_optional_membership_source(
    left: Option<&OperatorTree>,
    right: Option<&OperatorTree>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_membership_term(left, right),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn same_membership_sequence(left: &[OperatorTree], right: &[OperatorTree]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_membership_term(left, right))
}

fn same_membership_multiset(left: &[OperatorTree], right: &[OperatorTree]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut matched = vec![false; right.len()];
    for left_term in left {
        let Some(index) = right.iter().enumerate().position(|(index, right_term)| {
            !matched[index] && same_membership_term(left_term, right_term)
        }) else {
            return false;
        };
        matched[index] = true;
    }
    true
}

fn vectors_close(a: &[f32], b: &[f32]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= 1e-7 * x.abs().max(y.abs()) + 1e-9)
}
