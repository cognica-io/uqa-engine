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
        field: Option<&str>,
        query: &str,
        scoring: Option<TextScoringMode>,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        let scoring = Self::bound_text_scoring(scoring)?;
        let Some(field) = field else {
            return self.capture_all_fields(query, scoring, bytes);
        };
        self.context
            .text
            .validate_text_search_field(self.table, field)?;
        self.capture_field_scores(field, query, scoring, bytes)
    }

    fn capture_field_scores(
        &self,
        field: &str,
        query: &str,
        scoring: TextScoringMode,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
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

    fn capture_all_fields(
        &self,
        query: &str,
        scoring: TextScoringMode,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        let fields = self.all_field_text_fields(scoring)?;
        let mut output: Option<CapturedScoredEntries> = None;
        for field in fields {
            let next = self.capture_field_scores(&field, query, scoring, bytes / 3)?;
            output = Some(match output {
                Some(previous) => previous.merge_field_scores(
                    next,
                    bytes,
                    self.context.runtime.cancellation_token(),
                )?,
                None => next,
            });
        }
        Ok(output.expect("all-field validation requires an indexed field"))
    }

    pub(super) fn all_field_text_fields(
        &self,
        scoring: TextScoringMode,
    ) -> DriverResult<Vec<String>> {
        if matches!(
            scoring,
            TextScoringMode::CustomBM25(_) | TextScoringMode::CustomBayesianBM25(_)
        ) {
            return Err(super::SQLError::TypeMismatch(
                "explicit text scoring parameters require one concrete field".into(),
            ));
        }
        let fields = self.context.text.fts_fields_for_table(self.table)?;
        if fields.is_empty() {
            return Err(super::SQLError::TypeMismatch(format!(
                "text search: table `{}` has no text-indexed columns",
                self.table
            )));
        }
        Ok(fields)
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
