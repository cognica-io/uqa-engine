//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive lexical scoring primitives and ranking.

use super::{
    search_stats_for_terms, storage_sql_error, Arc, BM25Scorer, BayesianBM25Scorer, Engine,
    InvertedIndex, Ordering, PostingList, SQLError, ScoredEntry, Scorer, ScoringMode,
};

impl Engine {
    pub(super) fn build_text_scorer(
        mode: &ScoringMode,
        stats_arc: Arc<uqa_core::IndexStats>,
        query_term_count: usize,
    ) -> Result<Arc<dyn Scorer>, SQLError> {
        Ok(match mode {
            ScoringMode::BM25(p) => {
                p.validate()
                    .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
                Arc::new(BM25Scorer::new(*p, stats_arc))
            }
            ScoringMode::BayesianBM25(p) => Arc::new(
                BayesianBM25Scorer::new(p.scaled_for_query_terms(query_term_count), stats_arc)
                    .map_err(|error| SQLError::TypeMismatch(error.to_string()))?,
            ),
        })
    }

    pub(super) fn rank_top_k(pl: &PostingList, top_k: usize) -> Vec<ScoredEntry> {
        let entries: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
        Self::rank_scored_entries_top_k(entries, top_k)
    }

    pub(super) fn rank_scored_entries_top_k(
        mut entries: Vec<ScoredEntry>,
        top_k: usize,
    ) -> Vec<ScoredEntry> {
        if top_k == 0 {
            return Vec::new();
        }
        if top_k < entries.len() {
            entries.select_nth_unstable_by(top_k, Self::compare_scored_entry_desc);
            entries.truncate(top_k);
        }
        entries.sort_by(Self::compare_scored_entry_desc);
        entries.truncate(top_k);
        entries
    }

    pub(super) fn compare_scored_entry_desc(a: &ScoredEntry, b: &ScoredEntry) -> Ordering {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    }

    pub(super) fn score_single_text_term(
        index: &dyn InvertedIndex,
        field: &str,
        analyzed_terms: &[String],
        mode: &ScoringMode,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let term = &analyzed_terms[0];
        let mut cursor = index
            .posting_cursor(field, term)
            .map_err(|error| storage_sql_error("open term score cursor", error))?;
        let document_frequency = cursor.doc_freq();
        let term_doc_freqs = [document_frequency];
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &term_doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats, 1)?;
        let idf = scorer.idf(document_frequency);
        let scored_capacity = usize::try_from(document_frequency)
            .map_err(|_| SQLError::Internal("text document frequency exceeds usize".into()))?;
        let mut entries = Vec::with_capacity(scored_capacity);
        while let Some(entry) = cursor.current() {
            entries.push(ScoredEntry {
                doc_id: entry.doc_id,
                score: scorer.finalize_score(&[scorer.term_score_with_idf(
                    entry.term_freq,
                    entry.doc_length.max(entry.term_freq),
                    idf,
                )]),
            });
            cursor
                .advance()
                .map_err(|error| storage_sql_error("advance term score cursor", error))?;
        }
        Ok(entries)
    }

    pub(super) fn score_multiple_text_terms(
        index: &dyn InvertedIndex,
        field: &str,
        analyzed_terms: &[String],
        mode: &ScoringMode,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let mut cursors = index
            .posting_cursors_bulk(field, analyzed_terms)
            .map_err(|error| storage_sql_error("open term score cursors", error))?;
        let term_doc_freqs = cursors
            .iter()
            .map(|cursor| cursor.doc_freq())
            .collect::<Vec<_>>();
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &term_doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats, analyzed_terms.len())?;
        let term_idfs: Vec<f64> = term_doc_freqs
            .iter()
            .map(|doc_freq| scorer.idf(*doc_freq))
            .collect();
        let mut term_freqs = vec![0_u64; analyzed_terms.len()];
        let mut per_term = vec![0.0; analyzed_terms.len()];
        let mut entries = Vec::new();
        loop {
            let Some(doc_id) = cursors
                .iter()
                .filter_map(|cursor| cursor.current().map(|entry| entry.doc_id))
                .min()
            else {
                break;
            };
            term_freqs.fill(0);
            let mut candidate_length = None;
            for (term_index, cursor) in cursors.iter_mut().enumerate() {
                let Some(entry) = cursor.current().filter(|entry| entry.doc_id == doc_id) else {
                    continue;
                };
                let doc_length = entry.doc_length.max(entry.term_freq);
                if let Some(previous) = candidate_length {
                    if previous != doc_length {
                        return Err(SQLError::Internal(format!(
                            "inconsistent indexed document length for document {doc_id}: {previous} and {doc_length}"
                        )));
                    }
                } else {
                    candidate_length = Some(doc_length);
                }
                term_freqs[term_index] = entry.term_freq;
                cursor
                    .advance()
                    .map_err(|error| storage_sql_error("advance term score cursor", error))?;
            }
            let doc_length = candidate_length.ok_or_else(|| {
                SQLError::Internal(format!(
                    "text cursor merge found no posting for document {doc_id}"
                ))
            })?;
            for ((term_score, term_freq), idf) in
                per_term.iter_mut().zip(&term_freqs).zip(&term_idfs)
            {
                *term_score = scorer.term_score_with_idf(*term_freq, doc_length, *idf);
            }
            entries.push(ScoredEntry {
                doc_id,
                score: scorer.finalize_score(&per_term),
            });
        }
        Ok(entries)
    }

    pub(super) fn text_top_k_capabilities(
        &self,
        table: &str,
        field: &str,
        query: &str,
    ) -> Result<uqa_planner::TextTopKCapabilities, SQLError> {
        let Some(t) = self
            .try_query_table(table)
            .map_err(|error| storage_sql_error("resolve text-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let index = t.inverted_index.read();
        let analyzer = index.get_search_analyzer(field);
        let analyzed_terms = analyzer
            .analyze(query)
            .map_err(|error| storage_sql_error("analyze text query", error))?;
        let indexed_document_count = index
            .field_doc_count(field)
            .map_err(|error| storage_sql_error("read indexed document count", error))?;
        Ok(uqa_planner::TextTopKCapabilities {
            analyzed_term_count: analyzed_terms.len(),
            indexed_document_count,
        })
    }
}
