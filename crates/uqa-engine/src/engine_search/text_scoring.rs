//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive lexical scoring primitives and ranking.

use super::{
    block_max_scorer_fingerprint, raw_bm25_params, search_stats_for_terms, storage_sql_error, Arc,
    BM25Scorer, BTreeMap, BTreeSet, BayesianBM25Scorer, DocId, Engine, InvertedIndex, Ordering,
    PostingList, SQLError, ScoredEntry, Scorer, ScoringMode, DEFAULT_BLOCK_SIZE,
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
        let mut present_terms = BTreeMap::<DocId, Vec<(usize, u64)>>::new();
        let mut candidate_lengths = BTreeMap::<DocId, u64>::new();
        let mut term_doc_freqs = Vec::with_capacity(analyzed_terms.len());
        let cursors = index
            .posting_cursors_bulk(field, analyzed_terms)
            .map_err(|error| storage_sql_error("open term score cursors", error))?;
        for (term_index, mut cursor) in cursors.into_iter().enumerate() {
            term_doc_freqs.push(cursor.doc_freq());
            while let Some(entry) = cursor.current() {
                let doc_length = entry.doc_length.max(entry.term_freq);
                if let Some(previous) = candidate_lengths.insert(entry.doc_id, doc_length) {
                    if previous != doc_length {
                        return Err(SQLError::Internal(format!(
                            "inconsistent indexed document length for document {}: {previous} and {doc_length}",
                            entry.doc_id
                        )));
                    }
                }
                present_terms
                    .entry(entry.doc_id)
                    .or_default()
                    .push((term_index, entry.term_freq));
                cursor
                    .advance()
                    .map_err(|error| storage_sql_error("advance term score cursor", error))?;
            }
        }
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &term_doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats, analyzed_terms.len())?;
        let term_idfs: Vec<f64> = term_doc_freqs
            .iter()
            .map(|doc_freq| scorer.idf(*doc_freq))
            .collect();
        let mut per_term = Vec::with_capacity(analyzed_terms.len());
        Ok(candidate_lengths
            .into_iter()
            .map(|(doc_id, doc_length)| {
                per_term.clear();
                for idf in &term_idfs {
                    per_term.push(scorer.term_score_with_idf(0, doc_length, *idf));
                }
                if let Some(terms) = present_terms.get(&doc_id) {
                    for &(term_index, term_freq) in terms {
                        per_term[term_index] = scorer.term_score_with_idf(
                            term_freq,
                            doc_length,
                            term_idfs[term_index],
                        );
                    }
                }
                ScoredEntry {
                    doc_id,
                    score: scorer.finalize_score(&per_term),
                }
            })
            .collect())
    }

    pub(super) fn text_top_k_capabilities(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
    ) -> Result<uqa_planner::TextTopKCapabilities, SQLError> {
        let Some(t) = self
            .try_table(table)
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
        if analyzed_terms.len() < 2 {
            return Ok(uqa_planner::TextTopKCapabilities {
                analyzed_term_count: analyzed_terms.len(),
                indexed_document_count,
                valid_block_max: false,
            });
        }

        let mut frequencies = BTreeMap::<&str, u64>::new();
        for term in &analyzed_terms {
            if !frequencies.contains_key(term.as_str()) {
                frequencies.insert(
                    term.as_str(),
                    index
                        .doc_freq(field, term)
                        .map_err(|error| storage_sql_error("read document frequency", error))?,
                );
            }
        }
        let doc_freqs = analyzed_terms
            .iter()
            .map(|term| frequencies[term.as_str()])
            .collect::<Vec<_>>();
        let stats = search_stats_for_terms(index.as_ref(), field, &analyzed_terms, &doc_freqs)
            .map_err(|error| storage_sql_error("read field statistics", error))?;
        let fingerprint = block_max_scorer_fingerprint(raw_bm25_params(mode), &stats);
        let mut checked = BTreeSet::new();
        let mut saw_nonempty = false;
        let mut valid_block_max = true;
        for (term, doc_freq) in analyzed_terms.iter().zip(&doc_freqs) {
            if *doc_freq == 0 || !checked.insert(term) {
                continue;
            }
            saw_nonempty = true;
            let posting_len = usize::try_from(*doc_freq).map_err(|_| {
                SQLError::Internal("text-search document frequency exceeds usize".into())
            })?;
            let expected_blocks = posting_len.div_ceil(DEFAULT_BLOCK_SIZE);
            let persisted = index
                .persisted_block_max_scores(field, term, &fingerprint)
                .map_err(|error| storage_sql_error("read persisted block-max scores", error))?;
            if persisted.as_ref().map(Vec::len) != Some(expected_blocks) {
                valid_block_max = false;
                break;
            }
        }

        Ok(uqa_planner::TextTopKCapabilities {
            analyzed_term_count: analyzed_terms.len(),
            indexed_document_count,
            valid_block_max: saw_nonempty && valid_block_max,
        })
    }
}
