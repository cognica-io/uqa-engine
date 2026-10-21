//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restrict filter reads to completed non-filter support without collecting identities or changing branch/error order.

use super::{CancellationToken, CapturedScoredEntries, DriverResult, OperatorTree, SelectedIds};

pub(super) fn capture(
    parts: &[OperatorTree],
    bytes: usize,
    cancellation: &CancellationToken,
    leaf: &impl Fn(&OperatorTree, Option<&SelectedIds>, usize) -> DriverResult<CapturedScoredEntries>,
) -> DriverResult<Vec<CapturedScoredEntries>> {
    if !parts
        .iter()
        .any(|part| matches!(part, OperatorTree::Filter { source: None, .. }))
    {
        return parts
            .iter()
            .map(|part| super::capture(part, bytes, cancellation, leaf))
            .collect::<Vec<_>>()
            .into_iter()
            .collect();
    }
    let outputs = parts
        .iter()
        .map(|part| match part {
            OperatorTree::Filter { source: None, .. } => None,
            _ => Some(super::capture(part, bytes, cancellation, leaf)),
        })
        .collect::<Vec<_>>();
    let support = support(&outputs, bytes, cancellation);
    let candidates = support.as_ref().ok().and_then(Option::as_ref);
    let filters = parts
        .iter()
        .map(|part| match part {
            OperatorTree::Filter { source: None, .. } => Some(leaf(part, candidates, bytes)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let outputs = outputs
        .into_iter()
        .zip(filters)
        .map(|(output, filter)| {
            output
                .or(filter)
                .expect("each intersection branch is executed once")
        })
        .collect::<DriverResult<Vec<_>>>()?;
    support?;
    Ok(outputs)
}

fn support(
    outputs: &[Option<DriverResult<CapturedScoredEntries>>],
    bytes: usize,
    cancellation: &CancellationToken,
) -> DriverResult<Option<SelectedIds>> {
    if outputs.iter().flatten().any(Result::is_err) {
        return Ok(None);
    }
    let mut inputs = Vec::new();
    for output in outputs.iter().flatten() {
        match output {
            Ok(input) => inputs.push(input.replay()?.peekable()),
            Err(_) => return Ok(None),
        }
    }
    let Some(mut first) = inputs.pop() else {
        return Ok(None);
    };
    let ids = std::iter::from_fn(move || loop {
        let candidate = match first.next()? {
            Ok(entry) => entry.doc_id,
            Err(error) => return Some(Err(error)),
        };
        let mut present = true;
        for input in &mut inputs {
            loop {
                match input.peek()? {
                    Ok(entry) if entry.doc_id < candidate => {
                        input.next();
                    }
                    Ok(entry) => {
                        present &= entry.doc_id == candidate;
                        break;
                    }
                    Err(_) => {
                        return Some(Err(input.next().expect("peeked error").unwrap_err()));
                    }
                }
            }
        }
        if present {
            return Some(Ok(candidate));
        }
    });
    SelectedIds::capture(ids, bytes, cancellation).map(Some)
}
