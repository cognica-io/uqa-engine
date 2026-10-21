//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture complete score-only trees before document reads, without population-sized postings.

use super::{DriverResult, OperatorTree, PhysicalRetrievalDriver, TextScoringMode};
use crate::query::scored_input::{CapturedScoredEntries, ScoredEntryCapture};
use uqa_core::CancellationToken;

mod filters;
mod fusion;
mod positive;
#[cfg(test)]
mod tests;
mod tree;

impl PhysicalRetrievalDriver<'_> {
    pub(in crate::operator_tree) fn capture_scored_input(
        &self,
        tree: &OperatorTree,
    ) -> DriverResult<Option<CapturedScoredEntries>> {
        if !tree::complete_scored_tree(tree) {
            return Ok(None);
        }
        self.capture_tree(tree, self.context.runtime.work_mem_bytes()?)
            .map(Some)
    }

    fn capture_tree(
        &self,
        tree: &OperatorTree,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        tree::capture(
            tree,
            bytes,
            &self.context.runtime.cancellation_token(),
            &|tree, support, bytes| match tree {
                OperatorTree::Term {
                    field,
                    query,
                    scoring,
                    top_k: None,
                } => self.capture_text_leaf(field.as_deref(), query, *scoring, bytes),
                OperatorTree::Filter {
                    field,
                    predicate,
                    source,
                } => self.capture_filter(field, predicate, source.as_deref(), support, bytes),
                OperatorTree::IndexScan {
                    index_name,
                    field,
                    predicate,
                } => self.capture_index_scan(index_name, field, predicate, bytes),
                OperatorTree::Complement(source) => self.capture_complement(source, bytes),
                OperatorTree::KNN { .. }
                | OperatorTree::CalibratedVectorMatch { .. }
                | OperatorTree::CosineProbability(_) => {
                    self.capture_vector_postings(self.execute_posting_node(tree)?, bytes)
                }
                OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
                    self.capture_exact_fusion(signals, *base_rate, bytes)
                }
                OperatorTree::RobustPositiveEvidencePool {
                    signals,
                    alpha,
                    gating,
                    weights,
                    logit_min,
                    logit_max,
                    adaptive_weights,
                } => self.capture_positive_pool(
                    super::PositiveEvidencePoolExecution {
                        signals,
                        alpha: *alpha,
                        gating,
                        weights: weights.as_deref(),
                        logit_min: logit_min.as_deref(),
                        logit_max: logit_max.as_deref(),
                        adaptive_weights: *adaptive_weights,
                    },
                    bytes,
                ),
                _ => unreachable!("eligible complete retrieval leaf"),
            },
        )
    }

    fn capture_vector_postings(
        &self,
        postings: uqa_core::PostingList,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        // The vector owner selects its explicit K-candidate pool. Keep that selection and calibration, then retain the scores without forcing an unbounded sibling text population into a posting list.
        let mut output = ScoredEntryCapture::new(bytes, self.context.runtime.cancellation_token())?;
        for entry in postings {
            output.push(uqa_core::ScoredEntry {
                doc_id: entry.doc_id,
                score: entry.payload.score,
            })?;
        }
        output.finish()
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
        self.capture_mode_scores(field, query, &mode, bytes)
    }

    fn capture_mode_scores(
        &self,
        field: &str,
        query: &str,
        mode: &uqa_scoring::ScoringMode,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        let mut entries =
            ScoredEntryCapture::new(bytes, self.context.runtime.cancellation_token())?;
        self.context
            .text
            .visit_search_leaf(self.table, field, query, mode, &mut |entry| {
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
