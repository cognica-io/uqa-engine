//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture complete text-only trees before document reads, without population-sized postings.

use super::{DriverResult, OperatorTree, PhysicalRetrievalDriver, TextScoringMode};
use crate::query::scored_input::{CapturedScoredEntries, ScoredEntryCapture};
use uqa_core::CancellationToken;

#[cfg(test)]
mod tests;
mod tree;

impl PhysicalRetrievalDriver<'_> {
    pub(in crate::operator_tree) fn capture_text_scores(
        &self,
        tree: &OperatorTree,
    ) -> DriverResult<Option<CapturedScoredEntries>> {
        if !tree::complete_text_tree(tree) {
            return Ok(None);
        }
        tree::capture(
            tree,
            self.context.runtime.work_mem_bytes()?,
            &self.context.runtime.cancellation_token(),
            &|field, query, scoring, bytes| self.capture_text_leaf(field, query, scoring, bytes),
        )
        .map(Some)
    }

    fn capture_text_leaf(
        &self,
        field: &str,
        query: &str,
        scoring: Option<TextScoringMode>,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        let scoring = Self::bound_text_scoring(scoring)?;
        self.context
            .text
            .validate_text_search_field(self.table, field)?;
        let mode = self.text_scoring_mode(field, scoring)?;
        let mut entries =
            ScoredEntryCapture::new(bytes, self.context.runtime.cancellation_token())?;
        self.context
            .text
            .visit_search_leaf(self.table, field, query, &mode, &mut |entry| {
                entries.push(entry)
            })?;
        entries.finish()
    }

    pub(super) fn bound_text_scoring(
        scoring: Option<TextScoringMode>,
    ) -> DriverResult<TextScoringMode> {
        scoring.ok_or_else(|| {
            super::SQLError::Internal(
                "OperatorTree::Term reached PhysicalRetrievalDriver without bound text scoring"
                    .into(),
            )
        })
    }

    pub(super) fn text_scoring_mode(
        &self,
        field: &str,
        scoring: TextScoringMode,
    ) -> DriverResult<uqa_scoring::ScoringMode> {
        Ok(match scoring {
            TextScoringMode::BM25 => {
                uqa_scoring::ScoringMode::BM25(uqa_scoring::BM25Params::default())
            }
            TextScoringMode::BayesianBM25 => {
                uqa_scoring::ScoringMode::BayesianBM25(self.bayesian_params_for(field)?)
            }
            TextScoringMode::CustomBM25(params) => uqa_scoring::ScoringMode::BM25(params),
            TextScoringMode::CustomBayesianBM25(params) => {
                uqa_scoring::ScoringMode::BayesianBM25(params)
            }
        })
    }
}
