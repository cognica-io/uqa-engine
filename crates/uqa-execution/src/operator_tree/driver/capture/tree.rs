//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain complete text branches before folding their ordered score-only payloads.

use super::{
    CancellationToken, CapturedScoredEntries, DriverResult, OperatorTree, ScoredEntryCapture,
    TextScoringMode,
};

pub(super) fn complete_text_tree(tree: &OperatorTree) -> bool {
    match tree {
        OperatorTree::Empty | OperatorTree::Term { top_k: None, .. } => true,
        OperatorTree::Union(children) | OperatorTree::Intersect(children) => {
            children.iter().all(complete_text_tree)
        }
        _ => false,
    }
}

pub(super) fn capture(
    tree: &OperatorTree,
    bytes: usize,
    cancellation: &CancellationToken,
    leaf: &impl Fn(
        Option<&str>,
        &str,
        Option<TextScoringMode>,
        usize,
    ) -> DriverResult<CapturedScoredEntries>,
) -> DriverResult<CapturedScoredEntries> {
    match tree {
        OperatorTree::Term {
            field,
            query,
            scoring,
            top_k: None,
        } => leaf(field.as_deref(), query, *scoring, bytes),
        OperatorTree::Union(children) | OperatorTree::Intersect(children) => {
            // Every branch must run, including branches after an error or an empty intersection. Branch errors retain the same left-to-right precedence as the ordinary executor.
            let branch_bytes = bytes / children.len().saturating_add(2);
            let outcomes = children
                .iter()
                .map(|child| capture(child, branch_bytes, cancellation, leaf))
                .collect::<Vec<_>>();
            let branches = outcomes.into_iter().collect::<DriverResult<Vec<_>>>()?;
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
            "complete text capture requires score-only text operands".into(),
        )),
    }
}
