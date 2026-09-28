//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local scoring after a positional or other support predicate accepts a candidate.

use super::TextSearchError;
use crate::{BM25Params, BayesianBM25Params, ScoringMode};
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget},
    IndexStats,
};

enum CandidateMode {
    BM25(BM25Params),
    Bayesian(BayesianBM25Params),
}
impl CandidateMode {
    fn params(&self) -> &BM25Params {
        match self {
            Self::BM25(params) => params,
            Self::Bayesian(params) => &params.bm25,
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
    avg_doc_length: f64,
    idfs: BudgetedVec<f64>,
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

    /// Reserve query IDFs from the caller's shared runtime allowance.
    ///
    /// Frequencies arrive in emitted query order. Corpus scalars and scoring parameters stay inline, and unused index statistics are dropped. The callback covers construction and IDF preparation; partial owners are released on failure.
    pub fn new_budgeted(
        mode: &ScoringMode,
        stats: IndexStats,
        document_frequencies: &[u64],
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> Result<(), TextSearchError>,
    ) -> Result<Self, TextSearchError> {
        poll()?;
        let total_docs = stats.total_docs;
        let avg_doc_length = stats.avg_doc_length;
        drop(stats);
        let scorer = match mode {
            ScoringMode::BM25(params) => {
                params.validate()?;
                CandidateMode::BM25(*params)
            }
            ScoringMode::BayesianBM25(params) => {
                let params = params.scaled_for_query_terms(document_frequencies.len());
                crate::bayesian_bm25::validate_params(params, avg_doc_length)?;
                CandidateMode::Bayesian(params)
            }
        };
        let mut output = Self {
            scorer,
            avg_doc_length,
            idfs: BudgetedVec::new(budget),
        };
        output.idfs.reserve(document_frequencies.len())?;
        for frequency in document_frequencies {
            poll()?;
            output.idfs.push(crate::bm25::idf(total_docs, *frequency))?;
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
            sum += self.scorer.params().score_with_idf(
                self.avg_doc_length,
                *frequency,
                document_length,
                *idf,
            );
        }
        poll()?;
        Ok(self.scorer.finalize(sum))
    }
}

#[cfg(test)]
mod tests;
