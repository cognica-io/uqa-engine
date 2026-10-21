//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Robust pooling over complete replayable score inputs without population-sized maps.

use super::evidence::{
    peek_identity, validate_probabilities, visit_ordered_union, EvidenceVisitError,
};
use uqa_core::ScoredEntry;
use uqa_fusion::{LogitGating, RobustPositiveEvidencePool};
use uqa_storage::{StorageBackendError, StorageBackendResult};

#[cfg(test)]
mod tests;

/// Borrow the existing pool options; normalization and weights retain declaration order.
#[derive(Clone, Copy)]
pub struct PositiveEvidencePoolOptions<'a> {
    pub alpha: f64,
    pub gating: LogitGating,
    pub base_rate: Option<f64>,
    pub weights: Option<&'a [f64]>,
    pub adaptive_weights: bool,
    pub logit_min: Option<&'a [f64]>,
    pub logit_max: Option<&'a [f64]>,
}

/// Configuration validation precedes child execution in the native operator, and follows retained branch execution at the SQL boundary.
pub struct PreparedPositiveEvidencePool<'a> {
    fuser: RobustPositiveEvidencePool,
    options: PositiveEvidencePoolOptions<'a>,
    signal_count: usize,
}

impl<'a> PreparedPositiveEvidencePool<'a> {
    pub fn new(
        options: PositiveEvidencePoolOptions<'a>,
        signal_count: usize,
    ) -> StorageBackendResult<Self> {
        if signal_count == 0 {
            return Err(error("positive-evidence pool requires at least one signal"));
        }
        if !options.alpha.is_finite() || !(0.0..=1.0).contains(&options.alpha) {
            return Err(error(format!(
                "positive-evidence pool alpha must be finite and in [0, 1], got {}",
                options.alpha
            )));
        }
        if let Some(base_rate) = options.base_rate {
            if !base_rate.is_finite() || base_rate <= 0.0 || base_rate >= 1.0 {
                return Err(error(format!(
                    "positive-evidence pool base_rate must be finite and in (0, 1), got {base_rate}"
                )));
            }
        }
        let mut fuser = RobustPositiveEvidencePool::new(options.alpha)
            .map_err(error)?
            .with_logit_gating(options.gating);
        if let Some(base_rate) = options.base_rate {
            fuser = fuser.with_base_rate(base_rate).map_err(error)?;
        }
        fuser
            .validate_configuration(
                signal_count,
                options.weights,
                options.logit_min,
                options.logit_max,
            )
            .map_err(error)?;
        Ok(Self {
            fuser,
            options,
            signal_count,
        })
    }

    /// Every replay must produce the same ascending identities. Validate all probabilities, retain the last duplicate score, and emit the union with the complete declared signal count. Working memory is proportional to that count.
    pub fn visit<I, E>(
        &self,
        mut replay: impl FnMut(usize) -> Result<I, E>,
        mut visit: impl FnMut(ScoredEntry) -> Result<(), E>,
    ) -> Result<(), EvidenceVisitError<E>>
    where
        I: Iterator<Item = Result<ScoredEntry, E>>,
    {
        validate_probabilities(self.signal_count, &mut replay, "positive-evidence pool")?;
        let adaptive = if self.options.weights.is_none() && self.options.adaptive_weights {
            adaptive_weights(&self.fuser, self.signal_count, &mut replay)
                .map_err(EvidenceVisitError::Input)?
        } else {
            None
        };
        let weights = self.options.weights.or(adaptive.as_deref());
        visit_ordered_union(self.signal_count, replay, |doc_id, probabilities| {
            let score = self
                .fuser
                .fuse_configured(
                    probabilities,
                    weights,
                    self.options.logit_min,
                    self.options.logit_max,
                )
                .map_err(|cause| EvidenceVisitError::Fusion(error(cause)))?;
            visit(ScoredEntry { doc_id, score }).map_err(EvidenceVisitError::Output)
        })
    }
}

fn error(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(message.to_string())
}

fn unique_scores<I, E>(input: I) -> impl Iterator<Item = Result<f64, E>>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    let mut input = input.peekable();
    std::iter::from_fn(move || {
        let mut entry = match input.next()? {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        loop {
            match peek_identity(&mut input) {
                Ok(Some(id)) if id == entry.doc_id => {
                    let Some(Ok(next)) = input.next() else {
                        unreachable!("peeked score");
                    };
                    entry = next;
                }
                Ok(_) => return Some(Ok(entry.score)),
                Err(error) => return Some(Err(error)),
            }
        }
    })
}

pub(super) fn adaptive_weights<I, E>(
    fuser: &RobustPositiveEvidencePool,
    signal_count: usize,
    replay: &mut impl FnMut(usize) -> Result<I, E>,
) -> Result<Option<Vec<f64>>, E>
where
    I: Iterator<Item = Result<ScoredEntry, E>>,
{
    let mut spreads = Vec::with_capacity(signal_count);
    for signal in 0..signal_count {
        let mut count = 0usize;
        let sum = unique_scores(replay(signal)?)
            .map(|score| {
                score.map(|probability| {
                    count += 1;
                    fuser.gated_logit(probability)
                })
            })
            .sum::<Result<f64, E>>()?;
        if count < 2 {
            spreads.push(0.0);
            continue;
        }
        let mean = sum / count as f64;
        let variance = unique_scores(replay(signal)?)
            .map(|score| {
                score.map(|probability| {
                    let difference = fuser.gated_logit(probability) - mean;
                    difference * difference
                })
            })
            .sum::<Result<f64, E>>()?
            / count as f64;
        spreads.push(variance.sqrt());
    }
    let total = spreads.iter().sum::<f64>();
    if total <= f64::EPSILON {
        return Ok(None);
    }
    let uniform = 0.5 / signal_count as f64;
    Ok(Some(
        spreads
            .into_iter()
            .map(|spread| uniform + 0.5 * spread / total)
            .collect(),
    ))
}
