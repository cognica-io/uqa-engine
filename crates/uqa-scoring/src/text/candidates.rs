//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local scoring after a positional or other support predicate accepts a candidate.

use super::TextSearchError;
use crate::{BM25Scorer, BayesianBM25Scorer, Scorer, ScoringMode};
use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget, MemoryReservation},
    IndexStats,
};

enum CandidateMode {
    BM25(BM25Scorer),
    Bayesian(BayesianBM25Scorer),
}
impl CandidateMode {
    fn scorer(&self) -> &dyn Scorer {
        match self {
            Self::BM25(scorer) => scorer,
            Self::Bayesian(scorer) => scorer,
        }
    }
    fn finalize(&self, sum: f64) -> f64 {
        match self {
            Self::BM25(_) => sum,
            Self::Bayesian(scorer) => scorer.calibrate_raw_value(sum),
        }
    }
}

/// Scores aligned emitted query terms, including duplicates, after support acceptance.
pub struct TextCandidateScorer {
    scorer: CandidateMode,
    idfs: BudgetedVec<f64>,
    // The scorer drops its owned scalar statistics before this payload lease is released.
    _statistics: MemoryReservation,
}

impl TextCandidateScorer {
    pub fn new(
        mode: &ScoringMode,
        stats: IndexStats,
        document_frequencies: &[u64],
    ) -> Result<Self, TextSearchError> {
        Self::new_budgeted(
            mode,
            stats,
            document_frequencies,
            &MemoryBudget::new(usize::MAX),
            || Ok(()),
        )
    }

    /// Reserve query IDFs and scalar statistics from the caller's shared runtime allowance.
    ///
    /// Only scalar statistics are needed because frequencies arrive in emitted query order. Unused vocabulary maps are dropped, and the native scorer is stored inline. The callback covers construction and IDF preparation; partial owners are released on failure.
    pub fn new_budgeted(
        mode: &ScoringMode,
        stats: IndexStats,
        document_frequencies: &[u64],
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> Result<(), TextSearchError>,
    ) -> Result<Self, TextSearchError> {
        poll()?;
        let mut scalar = IndexStats::new(stats.total_docs);
        scalar.avg_doc_length = stats.avg_doc_length;
        scalar.dimensions = stats.dimensions;
        drop(stats);
        let memory = budget.reserve(size_of::<IndexStats>())?;
        let stats = Arc::new(scalar);
        let scorer = match mode {
            ScoringMode::BM25(params) => {
                params.validate()?;
                CandidateMode::BM25(BM25Scorer::new(*params, stats))
            }
            ScoringMode::BayesianBM25(params) => CandidateMode::Bayesian(BayesianBM25Scorer::new(
                params.scaled_for_query_terms(document_frequencies.len()),
                stats,
            )?),
        };
        let mut output = Self {
            scorer,
            idfs: BudgetedVec::new(budget),
            _statistics: memory,
        };
        output.idfs.reserve(document_frequencies.len())?;
        for frequency in document_frequencies {
            poll()?;
            output.idfs.push(output.scorer.scorer().idf(*frequency))?;
        }
        poll()?;
        Ok(output)
    }

    pub fn score_document(
        &mut self,
        document_length: u64,
        term_frequencies: &[u64],
    ) -> Result<f64, TextSearchError> {
        self.score_document_with_control(document_length, term_frequencies, || Ok(()))
    }

    /// Sum native term contributions in emitted order and apply calibration once, without a temporary score vector.
    pub fn score_document_with_control(
        &self,
        document_length: u64,
        term_frequencies: &[u64],
        mut poll: impl FnMut() -> Result<(), TextSearchError>,
    ) -> Result<f64, TextSearchError> {
        poll()?;
        if term_frequencies.len() != self.idfs.len() {
            return Err(TextSearchError::InvalidIndex(
                "candidate frequencies do not match the emitted query term count".into(),
            ));
        }
        // The scalar sum uses the same initial value and left-to-right order as Iterator::sum.
        let mut sum = -0.0;
        for (idf, frequency) in self.idfs.iter().zip(term_frequencies) {
            poll()?;
            sum += self
                .scorer
                .scorer()
                .term_score_with_idf(*frequency, document_length, *idf);
        }
        poll()?;
        Ok(self.scorer.finalize(sum))
    }
}

#[cfg(test)]
mod tests;
