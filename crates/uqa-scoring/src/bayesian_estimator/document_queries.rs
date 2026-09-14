//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic calibration queries from occurrence-preserving document analysis.

use super::UnsupervisedBm25ScoreEstimator;
use std::collections::BTreeSet;
use uqa_core::DocId;
use uqa_storage::TokenTermKey;

impl UnsupervisedBm25ScoreEstimator {
    /// Sample distinct terms from stride-selected documents at the estimator's calibration lengths. The consumer supplies document analysis while retaining its own snapshot and errors.
    pub fn sample_document_queries<E>(
        &self,
        mut doc_ids: Vec<DocId>,
        mut terms_for_document: impl FnMut(DocId) -> Result<Option<Vec<TokenTermKey>>, E>,
    ) -> Result<Vec<Vec<TokenTermKey>>, E> {
        doc_ids.sort_unstable();
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }

        let lengths = self.calibration_lengths();
        let target = self.n_samples();
        // Oversample document slots: short documents cannot fill the
        // longer query lengths and get skipped.
        let oversampled_target = target.saturating_mul(2).max(1);
        let stride = (doc_ids.len() / oversampled_target).max(1);
        let mut queries: Vec<Vec<TokenTermKey>> = Vec::new();
        let mut length_index = 0;
        let mut cursor = 0;
        while queries.len() < target && cursor < doc_ids.len() {
            let doc_id = doc_ids[cursor];
            cursor += stride;
            let Some(terms) = terms_for_document(doc_id)? else {
                continue;
            };
            let mut distinct: Vec<TokenTermKey> = Vec::new();
            let mut seen = BTreeSet::new();
            for term in terms {
                if seen.insert(term.clone()) {
                    distinct.push(term);
                }
            }
            let length = lengths[length_index % lengths.len()];
            if distinct.len() < length {
                continue;
            }
            distinct.truncate(length);
            queries.push(distinct);
            length_index += 1;
        }
        Ok(queries)
    }
}
