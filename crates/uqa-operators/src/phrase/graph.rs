//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memoized traversal of query and document occurrence graphs.

use super::{PhraseBudget, PhraseError, PhraseResult};
use uqa_core::TokenOccurrence;
use uqa_storage::TokenTermKey;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Edge {
    pub start: u32,
    pub end: u32,
    pub term: usize,
}

pub(super) struct QueryGraph<'a> {
    pub terms: Vec<&'a TokenTermKey>,
    pub emitted_terms: Vec<usize>,
    pub edges: Vec<Edge>,
    pub start: u32,
    pub end: u32,
}

impl<'a> QueryGraph<'a> {
    pub fn new(
        query: &'a [(TokenTermKey, TokenOccurrence)],
        budget: &mut PhraseBudget<'_>,
    ) -> PhraseResult<Self> {
        budget.reserve_items::<&TokenTermKey>(query.len())?;
        budget.reserve_items::<usize>(query.len())?;
        budget.reserve_items::<Edge>(query.len())?;
        let mut terms = query.iter().map(|(term, _)| term).collect::<Vec<_>>();
        terms.sort_unstable();
        terms.dedup();
        let mut edges = Vec::with_capacity(query.len());
        let mut emitted_terms = Vec::with_capacity(query.len());
        for (key, occurrence) in query {
            budget.check_cancelled()?;
            occurrence
                .validate()
                .map_err(|error| PhraseError::InvalidGraph(error.to_string()))?;
            let term = terms.binary_search(&key).expect("query term was collected");
            emitted_terms.push(term);
            edges.push(Edge {
                start: occurrence.position,
                end: occurrence.end_position().expect("validated edge"),
                term,
            });
        }
        edges.sort_unstable();
        edges.dedup();
        Ok(Self {
            start: edges.first().map_or(0, |edge| edge.start),
            end: edges.iter().map(|edge| edge.end).max().unwrap_or(0),
            terms,
            emitted_terms,
            edges,
        })
    }

    pub fn matches(
        &self,
        occurrences: &[Vec<TokenOccurrence>],
        states: &mut Vec<(u32, u32)>,
        budget: &mut PhraseBudget<'_>,
    ) -> PhraseResult<bool> {
        states.clear();
        for edge in self
            .edges
            .iter()
            .take_while(|edge| edge.start == self.start)
        {
            for occurrence in &occurrences[edge.term] {
                insert_state(
                    states,
                    (edge.end, occurrence.end_position().expect("validated edge")),
                    budget,
                )?;
            }
        }
        let mut current = 0;
        while let Some(&(query_position, document_position)) = states.get(current) {
            budget.check_cancelled()?;
            if query_position == self.end {
                return Ok(true);
            }
            let begin = self
                .edges
                .partition_point(|edge| edge.start < query_position);
            if let Some(next) = self.edges.get(begin) {
                if next.start > query_position {
                    // Removed tokens leave a real adjacency gap; no term can satisfy it early.
                    if let Some(document_next) =
                        document_position.checked_add(next.start - query_position)
                    {
                        insert_state(states, (next.start, document_next), budget)?;
                    }
                } else {
                    for edge in self.edges[begin..]
                        .iter()
                        .take_while(|edge| edge.start == query_position)
                    {
                        let postings = &occurrences[edge.term];
                        let begin =
                            postings.partition_point(|item| item.position < document_position);
                        for occurrence in postings[begin..]
                            .iter()
                            .take_while(|item| item.position == document_position)
                        {
                            insert_state(
                                states,
                                (edge.end, occurrence.end_position().expect("validated edge")),
                                budget,
                            )?;
                        }
                    }
                }
            }
            current += 1;
        }
        Ok(false)
    }
}

fn insert_state(
    states: &mut Vec<(u32, u32)>,
    state: (u32, u32),
    budget: &mut PhraseBudget<'_>,
) -> PhraseResult<()> {
    budget.check_cancelled()?;
    if let Err(at) = states.binary_search(&state) {
        budget.grow(states)?;
        // Query edges and hole transitions strictly advance, so new states follow the current state.
        states.insert(at, state);
    }
    Ok(())
}
