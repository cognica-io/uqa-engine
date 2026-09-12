//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connected phrase support over lossless query terms and native positional postings.

use graph::QueryGraph;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError},
    DocId, ScoredEntry, TokenOccurrence,
};
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
    Ok(score_phrase_budgeted(index, field, query, mode, budget)?
        .into_parts()
        .0)
}

/// Retain native graph, scoring, occurrence and output reservations through the returned result.
///
/// The borrowed query keeps its caller's existing ownership. Use the same allowance for complete query analysis so its key buffers coexist with matching state and results. Provider read workspaces remain with their provider implementation.
pub fn score_phrase_budgeted(
    index: &dyn InvertedIndex,
    field: &str,
    query: &[(TokenTermKey, TokenOccurrence)],
    mode: &ScoringMode,
    budget: &PhraseBudget<'_>,
) -> PhraseResult<Budgeted<Vec<ScoredEntry>>> {
    let (rows, memory) = score_phrase_inner(index, field, query, mode, budget)?.into_parts();
    Ok(Budgeted::new(rows, memory))
}

fn score_phrase_inner(
    index: &dyn InvertedIndex,
    field: &str,
    query: &[(TokenTermKey, TokenOccurrence)],
    mode: &ScoringMode,
    budget: &PhraseBudget<'_>,
) -> PhraseResult<BudgetedVec<ScoredEntry>> {
    budget.check_cancelled()?;
    if query.is_empty() {
        return Ok(BudgetedVec::new(budget.memory()));
    }
    let graph = QueryGraph::new(query, budget)?;
    let mut cursors: BudgetedVec<Box<dyn PostingReadCursor + '_>> =
        BudgetedVec::new(budget.memory());
    cursors.reserve(graph.terms.len())?;
    for term in graph.terms.iter() {
        budget.check_cancelled()?;
        cursors.push(index.posting_read_cursor_key(field, term)?)?;
    }
    let mut frequencies = BudgetedVec::new(budget.memory());
    frequencies.reserve(query.len())?;
    for &term in graph.emitted_terms.iter() {
        budget.check_cancelled()?;
        frequencies.push(cursors[term].doc_freq())?;
    }
    let scorer = TextCandidateScorer::new_budgeted(
        mode,
        index.field_stats_scalar(field)?,
        &frequencies,
        budget.memory(),
        || budget.cancellation().check().map_err(Into::into),
    )?;
    let mut occurrences = BudgetedVec::new(budget.memory());
    occurrences.reserve(graph.terms.len())?;
    for _ in graph.terms.iter() {
        budget.check_cancelled()?;
        occurrences.push(Budgeted::new(
            Vec::new(),
            budget.memory().empty_reservation(),
        ))?;
    }
    let mut states = BudgetedVec::new(budget.memory());
    let mut entries = BudgetedVec::new(budget.memory());
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
            for (frequency, &term) in frequencies.iter_mut().zip(graph.emitted_terms.iter()) {
                budget.check_cancelled()?;
                *frequency = cursors[term]
                    .current()
                    .filter(|entry| entry.doc_id == doc_id)
                    .map_or(0, |entry| entry.term_freq);
            }
            let score = scorer.score_document_with_control(length, &frequencies, || {
                budget.cancellation().check().map_err(Into::into)
            })?;
            entries.push(ScoredEntry { doc_id, score })?;
        }
        for values in &mut *occurrences {
            *values = Budgeted::new(Vec::new(), budget.memory().empty_reservation());
        }
        for cursor in &mut *cursors {
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
        let (mut entry, mut exit) = (None, None);
        for edge in graph.edges.iter() {
            budget.check_cancelled()?;
            if let Some(row) = cursors[edge.term].current() {
                if edge.start == graph.start {
                    entry = Some(entry.map_or(row.doc_id, |id: DocId| id.min(row.doc_id)));
                }
                if edge.end == graph.end {
                    exit = Some(exit.map_or(row.doc_id, |id: DocId| id.min(row.doc_id)));
                }
            }
        }
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
    occurrences: &mut [Budgeted<Vec<TokenOccurrence>>],
    budget: &PhraseBudget<'_>,
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
        let mut memory = budget.memory().reserve(
            count
                .checked_mul(size_of::<TokenOccurrence>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        let records = index.get_occurrences(doc_id, field, term)?;
        if records.len() != count {
            return Err(PhraseError::InvalidGraph(
                "phrase occurrences do not match the score cursor frequency".into(),
            ));
        }
        memory.grow(
            (records.capacity() - count)
                .checked_mul(size_of::<TokenOccurrence>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        *values = Budgeted::new(records, memory);
        let mut previous = None;
        for occurrence in values.iter() {
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
