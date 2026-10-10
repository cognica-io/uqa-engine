//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restrict residual document reads without moving filters across retrieval or reordering payload merges.

use super::{
    graph_execution_error, DocId, DriverResult, OperatorOutput, OperatorTree, PostingList,
    Predicate, SQLError,
};
use crate::parallel::ParallelExecutor;

pub(super) fn execute(
    parts: &[OperatorTree],
    parallel: &ParallelExecutor,
    execute: impl Fn(&OperatorTree) -> DriverResult<OperatorOutput> + Sync,
    filter: impl Fn(&str, &Predicate, Option<&[DocId]>) -> DriverResult<PostingList> + Sync,
) -> DriverResult<OperatorOutput> {
    let execute = &execute;
    let filter = &filter;
    let membership_only = parts.iter().all(OperatorTree::is_membership_only);
    if !parts
        .iter()
        .any(|part| matches!(part, OperatorTree::Filter { source: None, .. }))
    {
        let workers: Vec<_> = parts.iter().map(|part| move || execute(part)).collect();
        let outputs: DriverResult<Vec<_>> =
            parallel.execute_branches(&workers).into_iter().collect();
        return merge(outputs?, membership_only);
    }
    let workers: Vec<_> = parts
        .iter()
        .map(|part| {
            move || match part {
                OperatorTree::Filter { source: None, .. } => None,
                _ => Some(execute(part)),
            }
        })
        .collect();
    let outputs = parallel.execute_branches(&workers);
    // A failed or differently typed branch takes the ordinary path. In particular, a filter must still report an unknown field even when another branch is empty or fails.
    let candidates = support(&outputs);
    let candidates = candidates.as_deref();
    let workers: Vec<_> = parts
        .iter()
        .map(|part| {
            move || match part {
                OperatorTree::Filter {
                    field,
                    predicate,
                    source: None,
                } => Some(filter(field, predicate, candidates).map(OperatorOutput::Posting)),
                _ => None,
            }
        })
        .collect();
    let filters = parallel.execute_branches(&workers);
    // Preserve both error precedence and the original decorated payload fold. Only physical field reads have moved; no top-k or scoring boundary has changed.
    let outputs: DriverResult<Vec<_>> = outputs
        .into_iter()
        .zip(filters)
        .map(|(output, filter)| {
            output
                .or(filter)
                .expect("each intersection branch is executed once")
        })
        .collect();
    merge(outputs?, membership_only)
}

fn support(outputs: &[Option<DriverResult<OperatorOutput>>]) -> Option<Vec<DocId>> {
    let mut postings = Vec::new();
    for output in outputs.iter().flatten() {
        match output {
            Ok(OperatorOutput::Posting(posting)) => postings.push(posting),
            _ => return None,
        }
    }
    let smallest = postings.iter().min_by_key(|posting| posting.len())?;
    Some(
        smallest
            .entries()
            .iter()
            .filter_map(|entry| {
                postings
                    .iter()
                    .all(|posting| {
                        posting
                            .entries()
                            .binary_search_by_key(&entry.doc_id, |entry| entry.doc_id)
                            .is_ok()
                    })
                    .then_some(entry.doc_id)
            })
            .collect(),
    )
}

fn merge(outputs: Vec<OperatorOutput>, membership_only: bool) -> DriverResult<OperatorOutput> {
    let mut iter = outputs.into_iter();
    let Some(first) = iter.next() else {
        return Ok(PostingList::new().into());
    };
    iter.try_fold(first, |acc, next| match (acc, next) {
        (OperatorOutput::Posting(left), OperatorOutput::Posting(right)) => {
            Ok(OperatorOutput::Posting(if membership_only {
                left.merge_support_intersection_owned(&right)
            } else {
                left.merge_intersection_owned(&right)
            }))
        }
        (OperatorOutput::Graph(left), OperatorOutput::Graph(right)) => left
            .merge_intersection(&right)
            .map(OperatorOutput::Graph)
            .map_err(|error| graph_execution_error("GraphIntersect", error)),
        (OperatorOutput::Generalized(left), OperatorOutput::Generalized(right)) => {
            Ok(OperatorOutput::Generalized(left.merge_intersection(&right)))
        }
        _ => Err(SQLError::TypeMismatch(
            "Intersect operands must use the same posting-list carrier".into(),
        )),
    })
}

#[cfg(test)]
mod tests;
