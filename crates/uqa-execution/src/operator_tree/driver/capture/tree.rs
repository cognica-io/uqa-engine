//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain complete score-only branches before their original ordered payload fold.

use super::{
    CancellationToken, CapturedScoredEntries, DriverResult, OperatorTree, ScoredEntryCapture,
};
use crate::query::SelectedIds;

mod intersection;

pub(super) fn complete_scored_tree(tree: &OperatorTree) -> bool {
    match tree {
        OperatorTree::Empty
        | OperatorTree::Term { top_k: None, .. }
        | OperatorTree::IndexScan { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. } => true,
        OperatorTree::CosineProbability(source) => {
            matches!(source.as_ref(), OperatorTree::KNN { .. })
        }
        OperatorTree::Filter { source, .. } => source.as_deref().is_none_or(complete_scored_tree),
        OperatorTree::Complement(source) => complete_scored_tree(source),
        OperatorTree::Union(children) | OperatorTree::Intersect(children) => {
            children.iter().all(complete_scored_tree)
        }
        OperatorTree::BayesianEvidenceFusion { signals, .. }
        | OperatorTree::RobustPositiveEvidencePool { signals, .. } => {
            signals.iter().all(complete_scored_tree)
        }
        _ => false,
    }
}

pub(super) fn capture(
    tree: &OperatorTree,
    bytes: usize,
    cancellation: &CancellationToken,
    leaf: &impl Fn(&OperatorTree, Option<&SelectedIds>, usize) -> DriverResult<CapturedScoredEntries>,
) -> DriverResult<CapturedScoredEntries> {
    match tree {
        OperatorTree::Term { top_k: None, .. }
        | OperatorTree::Filter { .. }
        | OperatorTree::IndexScan { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::CosineProbability(_)
        | OperatorTree::Complement(_)
        | OperatorTree::BayesianEvidenceFusion { .. }
        | OperatorTree::RobustPositiveEvidencePool { .. } => leaf(tree, None, bytes),
        OperatorTree::Union(children) | OperatorTree::Intersect(children) => {
            // Every branch must run, including branches after an error or an empty intersection. Branch errors retain the same left-to-right precedence as the ordinary executor.
            let branch_bytes = bytes / children.len().saturating_add(2);
            let branches = if matches!(tree, OperatorTree::Intersect(_)) {
                intersection::capture(children, branch_bytes, cancellation, leaf)?
            } else {
                children
                    .iter()
                    .map(|child| capture(child, branch_bytes, cancellation, leaf))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .collect::<DriverResult<Vec<_>>>()?
            };
            let mut branches = branches.into_iter();
            let Some(first) = branches.next() else {
                return ScoredEntryCapture::new(bytes, cancellation.clone())?.finish();
            };
            branches.try_fold(first, |left, right| {
                left.merge_text_scores(
                    right,
                    matches!(tree, OperatorTree::Intersect(_)),
                    bytes / 2,
                    cancellation.clone(),
                )
            })
        }
        OperatorTree::Empty => ScoredEntryCapture::new(bytes, cancellation.clone())?.finish(),
        _ => Err(super::super::SQLError::Internal(
            "complete retrieval capture requires score-only operands".into(),
        )),
    }
}
