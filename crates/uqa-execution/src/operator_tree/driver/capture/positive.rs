//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain robust pooling inputs and output within the existing retrieval allowance.

use super::super::{
    combine_signal_priors, operator_execution_error, PositiveEvidencePoolExecution,
};
use super::{CapturedScoredEntries, DriverResult, PhysicalRetrievalDriver, ScoredEntryCapture};
use uqa_operators::hybrid::evidence::EvidenceVisitError;
use uqa_operators::hybrid::positive::{PositiveEvidencePoolOptions, PreparedPositiveEvidencePool};

#[cfg(test)]
mod tests;

impl PhysicalRetrievalDriver<'_> {
    pub(super) fn capture_positive_pool(
        &self,
        execution: PositiveEvidencePoolExecution<'_>,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        if execution.signals.is_empty() {
            return Err(super::super::SQLError::TypeMismatch(
                "RobustPositiveEvidencePool requires at least one signal".into(),
            ));
        }
        let inputs = self.capture_evidence_signals(execution.signals, bytes)?;
        // A single normalization bound is ignored at the established native boundary.
        let (logit_min, logit_max) = match (execution.logit_min, execution.logit_max) {
            (Some(minimum), Some(maximum)) => (Some(minimum), Some(maximum)),
            _ => (None, None),
        };
        pool_captured(
            inputs,
            PositiveEvidencePoolOptions {
                alpha: execution.alpha,
                gating: super::super::fusion::lower_positive_gating(execution.gating),
                base_rate: None,
                weights: execution.weights,
                adaptive_weights: execution.adaptive_weights,
                logit_min,
                logit_max,
            },
            bytes,
            self.context.runtime.cancellation_token(),
            || self.bridge_score_context(),
        )
    }
}

fn pool_captured<C>(
    inputs: Vec<(CapturedScoredEntries, Option<f64>)>,
    mut options: PositiveEvidencePoolOptions<'_>,
    bytes: usize,
    cancellation: uqa_core::CancellationToken,
    bridge: impl FnOnce() -> DriverResult<C>,
) -> DriverResult<CapturedScoredEntries> {
    let priors = inputs
        .iter()
        .filter_map(|(_, prior)| *prior)
        .collect::<Vec<_>>();
    options.base_rate = combine_signal_priors(&priors);
    let _context = bridge()?;
    let pool = PreparedPositiveEvidencePool::new(options, inputs.len())
        .map_err(|error| operator_execution_error("RobustPositiveEvidencePool", error))?;
    let mut output = ScoredEntryCapture::new(bytes / inputs.len().saturating_add(2), cancellation)?;
    pool.visit(
        |signal| inputs[signal].0.replay(),
        |entry| output.push(entry),
    )
    .map_err(|error| match error {
        EvidenceVisitError::Input(error) | EvidenceVisitError::Output(error) => error,
        EvidenceVisitError::Fusion(error) => {
            operator_execution_error("RobustPositiveEvidencePool", error)
        }
    })?;
    output.finish()
}
