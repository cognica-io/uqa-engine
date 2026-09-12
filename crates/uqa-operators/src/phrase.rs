//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connected phrase support over lossless query terms and native positional postings.

use graph::QueryGraph;
use uqa_core::{DocId, ScoredEntry, TokenOccurrence};
use uqa_scoring::{ScoringMode, TextCandidateScorer};
use uqa_storage::{clustered_postings::PostingReadCursor, InvertedIndex, TokenTermKey};

mod budget;
mod error;
mod graph;

pub use budget::PhraseBudget;
pub use error::{PhraseError, PhraseResult};

/// Match a complete analyzed phrase against one retained field index and score only accepted documents.
///
/// Candidate support is the intersection of the first-edge and last-edge posting unions. Query paths remain a graph; memoized query/document states preserve alternatives, holes, and independent edge lengths. Query occurrences retain their original multiplicity for scoring. Leading and trailing removed tokens impose no source anchoring.
pub fn score_phrase(
    index: &dyn InvertedIndex,
    field: &str,
    query: &[(TokenTermKey, TokenOccurrence)],
    mode: &ScoringMode,
    budget: &mut PhraseBudget<'_>,
) -> PhraseResult<Vec<ScoredEntry>> {
    let before = budget.used_bytes();
    let result = score_phrase_inner(index, field, query, mode, budget);
    let retained = result
        .as_ref()
        .map_or(0, |rows| rows.capacity() * size_of::<ScoredEntry>());
    budget.finish_scope(before, retained);
    result
}

fn score_phrase_inner(
    index: &dyn InvertedIndex,
    field: &str,
    query: &[(TokenTermKey, TokenOccurrence)],
    mode: &ScoringMode,
    budget: &mut PhraseBudget<'_>,
) -> PhraseResult<Vec<ScoredEntry>> {
    budget.check_cancelled()?;
    if query.is_empty() {
        return Ok(Vec::new());
    }
    budget.reserve_items::<(TokenTermKey, TokenOccurrence)>(query.len())?;
    for (term, _) in query {
        budget.reserve_bytes(term.as_bytes().len())?;
    }
    let graph = QueryGraph::new(query, budget)?;
    budget.reserve_items::<Box<dyn PostingReadCursor + '_>>(graph.terms.len())?;
    let mut cursors = graph
        .terms
        .iter()
        .map(|term| index.posting_read_cursor_key(field, term))
        .collect::<Result<Vec<_>, _>>()?;
    budget.reserve_items::<u64>(query.len())?;
    budget.reserve_items::<f64>(
        query
            .len()
            .checked_mul(2)
            .ok_or_else(|| PhraseError::InvalidGraph("query term count overflow".into()))?,
    )?;
    let mut frequencies = graph
        .emitted_terms
        .iter()
        .map(|&term| cursors[term].doc_freq())
        .collect::<Vec<_>>();
    let mut scorer =
        TextCandidateScorer::new(mode, index.field_stats_scalar(field)?, &frequencies)?;
    budget.reserve_items::<Vec<TokenOccurrence>>(graph.terms.len())?;
    let mut occurrences = (0..graph.terms.len())
        .map(|_| Vec::new())
        .collect::<Vec<_>>();
    let mut states = Vec::new();
    let mut entries = Vec::new();
    while let Some(doc_id) = next_candidate(&graph, &mut cursors, budget)? {
        budget.check_cancelled()?;
        let length = read_candidate(
            index,
            field,
            doc_id,
            &graph,
            &mut cursors,
            &mut occurrences,
            budget,
        )?;
        if graph.matches(&occurrences, &mut states, budget)? {
            for (frequency, &term) in frequencies.iter_mut().zip(&graph.emitted_terms) {
                *frequency = cursors[term]
                    .current()
                    .filter(|entry| entry.doc_id == doc_id)
                    .map_or(0, |entry| entry.term_freq);
            }
            let score = scorer.score_document(length, &frequencies)?;
            budget.grow(&mut entries)?;
            entries.push(ScoredEntry { doc_id, score });
        }
        for values in &mut occurrences {
            budget.release_bytes(values.capacity() * size_of::<TokenOccurrence>());
            *values = Vec::new();
        }
        for cursor in &mut cursors {
            budget.check_cancelled()?;
            if cursor.current().is_some_and(|entry| entry.doc_id == doc_id) {
                cursor.advance()?;
            }
        }
    }
    Ok(entries)
}

fn next_candidate(
    graph: &QueryGraph<'_>,
    cursors: &mut [Box<dyn PostingReadCursor + '_>],
    budget: &PhraseBudget<'_>,
) -> PhraseResult<Option<DocId>> {
    loop {
        budget.check_cancelled()?;
        let entry = graph
            .edges
            .iter()
            .filter(|edge| edge.start == graph.start)
            .filter_map(|edge| cursors[edge.term].current().map(|entry| entry.doc_id))
            .min();
        let exit = graph
            .edges
            .iter()
            .filter(|edge| edge.end == graph.end)
            .filter_map(|edge| cursors[edge.term].current().map(|entry| entry.doc_id))
            .min();
        let (Some(entry), Some(exit)) = (entry, exit) else {
            return Ok(None);
        };
        if entry == exit {
            return Ok(Some(entry));
        }
        let target = entry.max(exit);
        for cursor in cursors.iter_mut() {
            budget.check_cancelled()?;
            if cursor.current().is_some_and(|entry| entry.doc_id < target) {
                cursor.advance_to(target)?;
            }
        }
    }
}

fn read_candidate(
    index: &dyn InvertedIndex,
    field: &str,
    doc_id: DocId,
    graph: &QueryGraph<'_>,
    cursors: &mut [Box<dyn PostingReadCursor + '_>],
    occurrences: &mut [Vec<TokenOccurrence>],
    budget: &mut PhraseBudget<'_>,
) -> PhraseResult<u64> {
    let mut length = None;
    for ((term, cursor), values) in graph.terms.iter().zip(cursors).zip(occurrences) {
        budget.check_cancelled()?;
        if cursor.current().is_some_and(|entry| entry.doc_id < doc_id) {
            cursor.advance_to(doc_id)?;
        }
        let Some(entry) = cursor.current().filter(|entry| entry.doc_id == doc_id) else {
            continue;
        };
        if length.is_some_and(|length| length != entry.doc_length) {
            return Err(PhraseError::InvalidGraph(
                "inconsistent phrase candidate document length".into(),
            ));
        }
        length = Some(entry.doc_length);
        let count = usize::try_from(entry.term_freq).map_err(|_| {
            PhraseError::InvalidGraph("phrase occurrence count exceeds usize".into())
        })?;
        budget.reserve_items::<TokenOccurrence>(count)?;
        *values = index.get_occurrences(doc_id, field, term)?;
        if values.len() != count {
            return Err(PhraseError::InvalidGraph(
                "phrase occurrences do not match the score cursor frequency".into(),
            ));
        }
        budget.reserve_items::<TokenOccurrence>(values.capacity() - count)?;
        let mut previous = None;
        for occurrence in values {
            budget.check_cancelled()?;
            occurrence
                .validate()
                .map_err(|error| PhraseError::InvalidGraph(error.to_string()))?;
            if previous.is_some_and(|position| position > occurrence.position) {
                return Err(PhraseError::InvalidGraph(
                    "phrase occurrences are not ordered by position".into(),
                ));
            }
            previous = Some(occurrence.position);
        }
    }
    length.ok_or_else(|| PhraseError::InvalidGraph("phrase candidate has no posting".into()))
}

#[cfg(test)]
mod tests;
