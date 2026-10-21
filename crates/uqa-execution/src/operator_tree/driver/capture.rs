//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture complete single-field text scores before document reads, without population-sized postings.

use super::{DriverResult, OperatorTree, PhysicalRetrievalDriver, TextScoringMode};
use crate::query::scored_input::{CapturedScoredEntries, ScoredEntryCapture};

impl PhysicalRetrievalDriver<'_> {
    pub(in crate::operator_tree) fn capture_text_scores(
        &self,
        tree: &OperatorTree,
    ) -> DriverResult<Option<CapturedScoredEntries>> {
        let OperatorTree::Term {
            field: Some(field),
            query,
            scoring,
            top_k: None,
        } = tree
        else {
            return Ok(None);
        };
        let scoring = Self::bound_text_scoring(*scoring)?;
        self.context
            .text
            .validate_text_search_field(self.table, field)?;
        let mode = self.text_scoring_mode(field, scoring)?;
        let mut entries = ScoredEntryCapture::new(
            self.context.runtime.work_mem_bytes()?,
            self.context.runtime.cancellation_token(),
        )?;
        self.context
            .text
            .visit_search_leaf(self.table, field, query, &mode, &mut |entry| {
                entries.push(entry)
            })?;
        entries.finish().map(Some)
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
