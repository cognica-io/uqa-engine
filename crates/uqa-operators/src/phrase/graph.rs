//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memoized traversal of query and document occurrence graphs.

use super::{PhraseBudget, PhraseError, PhraseResult};
use uqa_core::{memory::BudgetedVec, ordering::sort_by_with_control, TokenOccurrence};
use uqa_storage::TokenTermKey;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Edge {
    pub start: u32,
    pub end: u32,
    pub term: usize,
}

pub(super) struct QueryGraph<'a> {
    pub terms: BudgetedVec<&'a TokenTermKey>,
    pub emitted_terms: BudgetedVec<usize>,
    pub edges: BudgetedVec<Edge>,
    pub start: u32,
    pub end: u32,
}

impl<'a> QueryGraph<'a> {
    pub fn new(
        query: &'a [(TokenTermKey, TokenOccurrence)],
        budget: &PhraseBudget<'_>,
    ) -> PhraseResult<Self> {
        let mut terms = BudgetedVec::new(budget.memory());
        terms.reserve(query.len())?;
        for (key, _) in query {
            budget.check_cancelled()?;
            terms.push(key)?;
        }
        sort_by_with_control(
            &mut terms,
            &mut || budget.check_cancelled(),
            |left, right, poll| left.cmp_with_control(right, poll),
        )?;
        let mut retained = 0;
        for index in 0..terms.len() {
            budget.check_cancelled()?;
            if retained == 0
                || !terms[index]
                    .cmp_with_control(terms[retained - 1], &mut || budget.check_cancelled())?
                    .is_eq()
            {
                terms.swap(retained, index);
                retained += 1;
            }
        }
        terms.truncate(retained);
        let mut edges = BudgetedVec::new(budget.memory());
        edges.reserve(query.len())?;
        let mut emitted_terms = BudgetedVec::new(budget.memory());
        emitted_terms.reserve(query.len())?;
        for (key, occurrence) in query {
            budget.check_cancelled()?;
            occurrence
                .validate()
                .map_err(|error| PhraseError::InvalidGraph(error.to_string()))?;
            let term = term_index(&terms, key, budget)?;
            emitted_terms.push(term)?;
            edges.push(Edge {
                start: occurrence.position,
                end: occurrence.end_position().expect("validated edge"),
                term,
            })?;
        }
        sort_by_with_control(
            &mut edges,
            &mut || budget.check_cancelled(),
            |left, right, _| Ok(left.cmp(right)),
        )?;
        let mut retained = 0;
        let mut end = 0;
        for index in 0..edges.len() {
            budget.check_cancelled()?;
            if retained == 0 || edges[index] != edges[retained - 1] {
                edges[retained] = edges[index];
                retained += 1;
            }
            end = end.max(edges[index].end);
        }
        edges.truncate(retained);
        Ok(Self {
            start: edges.first().map_or(0, |edge| edge.start),
            end,
            terms,
            emitted_terms,
            edges,
        })
    }

    pub fn matches(
        &self,
        occurrences: &[impl AsRef<[TokenOccurrence]>],
        states: &mut BudgetedVec<(u32, u32)>,
        budget: &PhraseBudget<'_>,
    ) -> PhraseResult<bool> {
        states.clear();
        for edge in self
            .edges
            .iter()
            .take_while(|edge| edge.start == self.start)
        {
            for occurrence in occurrences[edge.term].as_ref() {
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
                        let postings = occurrences[edge.term].as_ref();
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
    states: &mut BudgetedVec<(u32, u32)>,
    state: (u32, u32),
    budget: &PhraseBudget<'_>,
) -> PhraseResult<()> {
    budget.check_cancelled()?;
    if let Err(at) = states.binary_search(&state) {
        states.push(state)?;
        // Query edges and holes advance strictly; insertion preserves the pending traversal order.
        for index in (at + 1..states.len()).rev() {
            budget.check_cancelled()?;
            states.swap(index - 1, index);
        }
    }
    Ok(())
}

fn term_index(
    terms: &[&TokenTermKey],
    key: &TokenTermKey,
    budget: &PhraseBudget<'_>,
) -> PhraseResult<usize> {
    let (mut start, mut end) = (0, terms.len());
    while start < end {
        budget.check_cancelled()?;
        let middle = start + (end - start) / 2;
        match terms[middle].cmp_with_control(key, &mut || budget.check_cancelled())? {
            std::cmp::Ordering::Less => start = middle + 1,
            std::cmp::Ordering::Greater => end = middle,
            std::cmp::Ordering::Equal => return Ok(middle),
        }
    }
    unreachable!("query term was collected")
}
