//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, PostingEntry};
use uqa_storage::inverted_index::analyze_query_graph_budgeted;

#[test]
fn query_and_field_results_keep_one_allowance_through_posting_publication() {
    let index = fixture(
        whitespace_analyzer(),
        &[(1, "a b"), (3, "b a"), (7, "a a b")],
    );
    let revision = index.search_analyzer_revision("body").unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let unrelated = memory.reserve(7).unwrap();
    let cancellation = CancellationToken::new();
    let budget = PhraseBudget::with_memory(&memory, &cancellation);
    let graph = analyze_query_graph_budgeted(&revision, "a b", &memory, || Ok(())).unwrap();
    assert_eq!(memory.used(), 7 + graph.reserved_bytes());
    let mode = ScoringMode::BM25(BM25Params::default());
    let first = score_phrase_budgeted(&index, "body", &graph, &mode, &budget).unwrap();
    let second = score_phrase_budgeted(&index, "body", &graph, &mode, &budget).unwrap();
    assert!(first
        .iter()
        .zip(second.iter())
        .all(|(left, right)| left.doc_id == right.doc_id
            && left.score.to_bits() == right.score.to_bits()));
    assert_eq!(first.len(), second.len());
    assert_eq!(
        first.iter().map(|row| row.doc_id).collect::<Vec<_>>(),
        [1, 7]
    );
    assert_eq!(
        memory.used(),
        7 + graph.reserved_bytes() + first.reserved_bytes() + second.reserved_bytes()
    );
    let mut rows = BudgetedVec::new(&memory);
    budget.append_results(&mut rows, first).unwrap();
    budget.append_results(&mut rows, second).unwrap();
    drop(graph);
    assert_eq!(
        memory.used(),
        7 + rows.capacity() * size_of::<ScoredEntry>()
    );
    rows.reverse();
    let postings = budget.finish_postings(rows).unwrap();
    let expected = search(&index, "a b");
    assert_eq!(postings.len(), expected.len());
    for (posting, row) in postings.iter().zip(expected) {
        assert_eq!(posting.doc_id, row.doc_id);
        assert_eq!(posting.payload.score.to_bits(), row.score.to_bits());
    }
    assert_eq!(memory.used(), 7 + postings.reserved_bytes());
    assert_eq!(
        postings.reserved_bytes(),
        postings.len() * size_of::<PostingEntry>()
    );
    drop(postings);
    assert_eq!(memory.used(), 7);
    drop(unrelated);
    assert_eq!(memory.used(), 0);
}

fn bounded_pipeline(
    index: &dyn InvertedIndex,
    memory: &MemoryBudget,
) -> PhraseResult<Budgeted<uqa_core::PostingList>> {
    let cancellation = CancellationToken::new();
    let budget = PhraseBudget::with_memory(memory, &cancellation);
    let revision = index.search_analyzer_revision("body")?;
    let graph = analyze_query_graph_budgeted(&revision, "a b", memory, || Ok(()))?;
    let matches = score_phrase_budgeted(index, "body", &graph, &ScoringMode::default(), &budget)?;
    drop(graph);
    let mut rows = BudgetedVec::new(memory);
    budget.append_results(&mut rows, matches)?;
    budget.finish_postings(rows)
}

#[test]
fn every_byte_limit_releases_only_failed_phrase_work_and_preserves_prior_results() {
    let index = fixture(whitespace_analyzer(), &[(1, "a b"), (7, "a a b")]);
    let baseline = MemoryBudget::new(1 << 20);
    let expected = bounded_pipeline(&index, &baseline).unwrap();
    let peak = baseline.peak();
    for limit in 0..=peak {
        let memory = MemoryBudget::new(limit + 7);
        let unrelated = memory.reserve(7).unwrap();
        match bounded_pipeline(&index, &memory) {
            Ok(actual) => {
                assert_eq!(*actual, *expected);
                assert_eq!(memory.used(), 7 + actual.reserved_bytes());
                drop(actual);
            }
            Err(
                PhraseError::MemoryLimit { .. }
                | PhraseError::Storage(uqa_storage::StorageBackendError::Analysis(
                    uqa_analysis::AnalysisError::Memory(MemoryError::Limit { .. }),
                )),
            ) => {}
            Err(error) => panic!("unexpected failure for limit {limit}: {error}"),
        }
        assert_eq!(memory.used(), 7, "limit {limit}");
        drop(unrelated);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(1 << 20);
    let prior = bounded_pipeline(&index, &memory).unwrap();
    let held = memory.used();
    let rest = memory.reserve(memory.limit() - held).unwrap();
    assert!(bounded_pipeline(&index, &memory).is_err());
    assert_eq!(*prior, *expected);
    assert_eq!(memory.used(), memory.limit());
    drop(rest);
    assert_eq!(memory.used(), held);
    drop(prior);
    assert_eq!(memory.used(), 0);
}

#[test]
fn cancelled_matching_keeps_live_query_and_previous_result_owners() {
    let index = fixture(whitespace_analyzer(), &[(1, "a b")]);
    let memory = MemoryBudget::new(1 << 20);
    let prior = bounded_pipeline(&index, &memory).unwrap();
    let revision = index.search_analyzer_revision("body").unwrap();
    let graph = analyze_query_graph_budgeted(&revision, "a b", &memory, || Ok(())).unwrap();
    let retained = memory.used();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let budget = PhraseBudget::with_memory(&memory, &cancellation);
    assert!(matches!(
        score_phrase_budgeted(&index, "body", &graph, &ScoringMode::default(), &budget),
        Err(PhraseError::Cancelled(_))
    ));
    assert_eq!(memory.used(), retained);
    assert_eq!(prior.len(), 1);
    drop(graph);
    drop(prior);
    assert_eq!(memory.used(), 0);
}
