//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded inputs at the exact evidence boundary, before document delivery.

use super::super::{combine_signal_priors, operator_execution_error, SQLError};
use super::{
    CapturedScoredEntries, DriverResult, OperatorTree, PhysicalRetrievalDriver, ScoredEntryCapture,
    TextScoringMode,
};
use uqa_fusion::BayesianEvidenceFusion;
use uqa_operators::hybrid::evidence::{visit_exact_evidence, EvidenceVisitError};
use uqa_scoring::ScoringMode;

#[cfg(test)]
mod tests;

impl PhysicalRetrievalDriver<'_> {
    pub(super) fn capture_exact_fusion(
        &self,
        signals: &[OperatorTree],
        explicit_base_rate: Option<f64>,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "BayesianEvidenceFusion requires at least one signal".into(),
            ));
        }
        let branch_bytes = bytes / signals.len().saturating_add(2);
        let workers = signals
            .iter()
            .map(|signal| {
                move || self.with_diagnostics(|| self.capture_fusion_signal(signal, branch_bytes))
            })
            .collect::<Vec<_>>();
        // All branches finish before the original first-error selection and prior validation.
        let inputs = self
            .parallel
            .execute_branches(&workers)
            .into_iter()
            .collect::<DriverResult<Vec<_>>>()?;
        fuse_captured(
            inputs,
            explicit_base_rate,
            bytes,
            self.context.runtime.cancellation_token(),
            || self.bridge_context(),
        )
    }

    fn capture_fusion_signal(
        &self,
        signal: &OperatorTree,
        bytes: usize,
    ) -> DriverResult<(CapturedScoredEntries, Option<f64>)> {
        match signal {
            OperatorTree::Term {
                query,
                field,
                scoring: Some(TextScoringMode::BayesianBM25),
                top_k: None,
            } => self.capture_bayesian_evidence(query, field.as_deref(), bytes),
            other => self.capture_tree(other, bytes).map(|rows| (rows, None)),
        }
    }

    fn capture_bayesian_evidence(
        &self,
        query: &str,
        field: Option<&str>,
        bytes: usize,
    ) -> DriverResult<(CapturedScoredEntries, Option<f64>)> {
        if let Some(field) = field {
            self.context
                .text
                .validate_text_search_field(self.table, field)?;
            let params = self.bayesian_params_for(field)?;
            let prior = (params.base_rate > 0.0).then_some(params.base_rate);
            let mode = ScoringMode::BayesianBM25(params.evidence_params());
            return self
                .capture_mode_scores(field, query, &mode, bytes)
                .map(|rows| (rows, prior));
        }
        let fields = self.all_field_text_fields(TextScoringMode::BayesianBM25)?;
        let mut priors = Vec::with_capacity(fields.len());
        let mut output: Option<CapturedScoredEntries> = None;
        for field in fields {
            let params = self.bayesian_params_for(&field)?;
            if params.base_rate > 0.0 {
                priors.push(params.base_rate);
            }
            let mode = ScoringMode::BayesianBM25(params.evidence_params());
            let next = self.capture_mode_scores(&field, query, &mode, bytes / 3)?;
            output = Some(match output {
                Some(previous) => previous.merge_field_scores(
                    next,
                    bytes,
                    self.context.runtime.cancellation_token(),
                )?,
                None => next,
            });
        }
        Ok((
            output.expect("all-field validation requires an indexed field"),
            combine_signal_priors(&priors),
        ))
    }
}

fn fuse_captured<C>(
    inputs: Vec<(CapturedScoredEntries, Option<f64>)>,
    explicit_base_rate: Option<f64>,
    bytes: usize,
    cancellation: uqa_core::CancellationToken,
    bridge: impl FnOnce() -> DriverResult<C>,
) -> DriverResult<CapturedScoredEntries> {
    let priors = inputs
        .iter()
        .filter_map(|(_, prior)| *prior)
        .collect::<Vec<_>>();
    let base_rate = match explicit_base_rate {
        Some(prior) => prior,
        None => super::super::fusion::resolve_exact_signal_prior(&priors)?.unwrap_or(0.5),
    };
    let _context = bridge()?;
    let fusion = BayesianEvidenceFusion::new(base_rate)
        .map_err(|error| operator_execution_error("BayesianEvidenceFusion", error))?;
    let mut output = ScoredEntryCapture::new(bytes / 2, cancellation)?;
    visit_exact_evidence(
        &fusion,
        inputs.len(),
        |signal| inputs[signal].0.replay(),
        |entry| output.push(entry),
    )
    .map_err(|error| match error {
        EvidenceVisitError::Input(error) | EvidenceVisitError::Output(error) => error,
        EvidenceVisitError::Fusion(error) => {
            operator_execution_error("BayesianEvidenceFusion", error)
        }
    })?;
    output.finish()
}
