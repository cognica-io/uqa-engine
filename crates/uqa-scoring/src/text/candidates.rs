//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local scoring after a positional or other support predicate has accepted a candidate.

use super::{exhaustive::build_text_scorer, TextSearchError};
use crate::{Scorer, ScoringMode};
use std::sync::Arc;
use uqa_core::IndexStats;

/// Scores aligned query occurrences without retrieving or limiting document support.
///
/// Document frequencies and term frequencies use the same emitted query order, including duplicates. Callers may filter support before scoring without changing BM25 or Bayesian query-length calibration.
pub struct TextCandidateScorer {
    scorer: Arc<dyn Scorer>,
    idfs: Vec<f64>,
    scores: Vec<f64>,
}

impl TextCandidateScorer {
    pub fn new(
        mode: &ScoringMode,
        stats: IndexStats,
        document_frequencies: &[u64],
    ) -> Result<Self, TextSearchError> {
        let scorer = build_text_scorer(mode, Arc::new(stats), document_frequencies.len())?;
        let idfs = document_frequencies
            .iter()
            .map(|df| scorer.idf(*df))
            .collect();
        Ok(Self {
            scorer,
            idfs,
            scores: vec![0.0; document_frequencies.len()],
        })
    }

    pub fn score_document(
        &mut self,
        document_length: u64,
        term_frequencies: &[u64],
    ) -> Result<f64, TextSearchError> {
        if term_frequencies.len() != self.idfs.len() {
            return Err(TextSearchError::InvalidIndex(
                "candidate frequencies do not match the emitted query term count".into(),
            ));
        }
        for ((score, idf), frequency) in
            self.scores.iter_mut().zip(&self.idfs).zip(term_frequencies)
        {
            *score = self
                .scorer
                .term_score_with_idf(*frequency, document_length, *idf);
        }
        Ok(self.scorer.finalize_score(&self.scores))
    }
}
