//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact evidence fusion over repeatable ordered score inputs.

use std::iter::Peekable;
use uqa_core::{DocId, ScoredEntry};
use uqa_fusion::BayesianEvidenceFusion;
use uqa_scoring::EvidenceLogit;
use uqa_storage::StorageBackendError;

#[cfg(test)]
mod tests;

/// Keep input and consumer failures in the caller's error domain.
#[derive(Debug)]
pub enum EvidenceVisitError<E> {
    Input(E),
    Output(E),
    Fusion(StorageBackendError),
}

/// Validate every probability before emitting output, then fuse the union in ascending identity order. Each replay must return the same ascending input; repeated identities retain their last score, as in the materialized score map. Memory is proportional to the number of signals, not their populations.
pub fn visit_exact_evidence<I, E>(
    fusion: &BayesianEvidenceFusion,
    signal_count: usize,
    mut replay: impl FnMut(usize) -> Result<I, E>,
    mut visit: impl FnMut(ScoredEntry) -> Result<(), E>,
) -> Result<(), EvidenceVisitError<E>>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    validate_probabilities(signal_count, &mut replay, "Bayesian evidence fusion")?;
    let mut evidence = Vec::with_capacity(signal_count);
    visit_ordered_union(signal_count, replay, |doc_id, probabilities| {
        evidence.clear();
        for probability in probabilities.iter().flatten() {
            evidence.push(
                EvidenceLogit::from_prior_free_probability(*probability).map_err(|error| {
                    EvidenceVisitError::Fusion(StorageBackendError::Other(error.to_string()))
                })?,
            );
        }
        let posterior = fusion.fuse(&evidence).map_err(|error| {
            EvidenceVisitError::Fusion(StorageBackendError::Other(error.to_string()))
        })?;
        visit(ScoredEntry {
            doc_id,
            score: posterior.value(),
        })
        .map_err(EvidenceVisitError::Output)
    })
}

pub(super) fn validate_probabilities<I, E>(
    signal_count: usize,
    replay: &mut impl FnMut(usize) -> Result<I, E>,
    operation: &str,
) -> Result<(), EvidenceVisitError<E>>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    for signal in 0..signal_count {
        for entry in replay(signal).map_err(EvidenceVisitError::Input)? {
            let entry = entry.map_err(EvidenceVisitError::Input)?;
            crate::base::require_probability(entry.score, operation)
                .map_err(EvidenceVisitError::Fusion)?;
        }
    }
    Ok(())
}

pub(super) fn visit_ordered_union<I, E>(
    signal_count: usize,
    mut replay: impl FnMut(usize) -> Result<I, E>,
    mut visit: impl FnMut(DocId, &[Option<f64>]) -> Result<(), EvidenceVisitError<E>>,
) -> Result<(), EvidenceVisitError<E>>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    let mut inputs = (0..signal_count)
        .map(|signal| {
            replay(signal)
                .map(Iterator::peekable)
                .map_err(EvidenceVisitError::Input)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut probabilities = vec![None; signal_count];
    loop {
        let mut next: Option<DocId> = None;
        for input in &mut inputs {
            if let Some(id) = peek_identity(input).map_err(EvidenceVisitError::Input)? {
                next = Some(next.map_or(id, |old| old.min(id)));
            }
        }
        let Some(doc_id) = next else { return Ok(()) };
        for (input, probability) in inputs.iter_mut().zip(&mut probabilities) {
            *probability = None;
            while peek_identity(input).map_err(EvidenceVisitError::Input)? == Some(doc_id) {
                *probability = Some(
                    input
                        .next()
                        .expect("peeked score")
                        .map_err(EvidenceVisitError::Input)?
                        .score,
                );
            }
        }
        visit(doc_id, &probabilities)?;
    }
}

pub(super) fn peek_identity<I, E>(input: &mut Peekable<I>) -> Result<Option<DocId>, E>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    match input.peek() {
        Some(Ok(entry)) => Ok(Some(entry.doc_id)),
        Some(Err(_)) => Err(input.next().expect("peeked input error").unwrap_err()),
        None => Ok(None),
    }
}
