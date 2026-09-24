//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::inverted_index::{AnalyzerPhase, MemoryPosting, PostingKey};
use crate::TokenTermKey;
use std::collections::BTreeMap;
use uqa_core::memory::{OwnedMap, OwnedSet};
use uqa_core::{memory::MemoryBudget, TokenOccurrence};

fn builder(control: &StorageReadControl) -> RetainedInvertedIndexBuilder {
    RetainedInvertedIndexBuilder::new(
        &AnalyzerBindings::new(uqa_analysis::whitespace_analyzer()),
        control,
    )
    .unwrap()
}

fn stage_selected<'a>(
    index: &MemoryInvertedIndex,
    doc_id: DocId,
    fields: impl IntoIterator<Item = (&'a str, &'a str)>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    let mut borrowed = BudgetedMap::new(control.memory());
    for (field, text) in fields {
        borrowed.insert(field, text)?;
    }
    staging::stage(index, doc_id, &borrowed, control)
}

pub(in crate::inverted_index) fn corpus_bytes(state: &MemoryIndexState) -> usize {
    size_of::<MemoryIndexState>()
        + state
            .index
            .iter()
            .map(|((field, term), postings)| {
                OwnedMap::<PostingKey, OwnedMap<DocId, MemoryPosting>>::entry_bytes()
                    + field.capacity()
                    + term.allocated_bytes()
                    + postings
                        .values()
                        .map(|posting| {
                            OwnedMap::<DocId, MemoryPosting>::entry_bytes()
                                + posting.projection.payload.positions.capacity() * size_of::<u32>()
                                + posting.occurrences.capacity() * size_of::<TokenOccurrence>()
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
        + state
            .doc_terms
            .values()
            .map(|terms| {
                OwnedMap::<DocId, OwnedSet<PostingKey>>::entry_bytes()
                    + terms
                        .iter()
                        .map(|(field, term)| {
                            OwnedSet::<PostingKey>::entry_bytes()
                                + field.capacity()
                                + term.allocated_bytes()
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
        + state
            .doc_fields
            .values()
            .map(|fields| {
                OwnedMap::<DocId, OwnedMap<FieldName, IndexedFieldMetadata>>::entry_bytes()
                    + fields
                        .keys()
                        .map(|field| {
                            OwnedMap::<FieldName, IndexedFieldMetadata>::entry_bytes()
                                + field.capacity()
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
        + [&state.total_length, &state.field_doc_counts]
            .into_iter()
            .map(|fields| {
                fields
                    .keys()
                    .map(|field| OwnedMap::<FieldName, u64>::entry_bytes() + field.capacity())
                    .sum::<usize>()
            })
            .sum::<usize>()
}

#[test]
fn retained_corpus_preserves_revisions_overlaps_field_counts_and_last_duplicate_field() {
    let control = StorageReadControl::with_limit(1 << 20);
    let config: uqa_analysis::Analyzer = serde_json::from_str(
        r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#,
    ).unwrap();
    let revision = uqa_analysis::AnalyzerResources::default()
        .compile_with_length_policy(&config, uqa_analysis::TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    let search = uqa_analysis::keyword_analyzer().compile().unwrap();
    let mut bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    bindings
        .bind_revisions("body", revision.clone(), search.clone())
        .unwrap();
    let mut ordinary = MemoryInvertedIndex::with_bindings(bindings.clone());
    let mut retained = RetainedInvertedIndexBuilder::new(&bindings, &control).unwrap();
    let binding_bytes = control.memory().used() - corpus_bytes(&retained.index.state);
    for (id, fields) in [
        (7, vec![("body", "a a β"), ("empty", "")]),
        (2, vec![("body", "discarded"), ("body", "a β")]),
        (9, vec![("body", "")]),
    ] {
        ordinary
            .add_document(
                id,
                fields
                    .iter()
                    .map(|&(field, text)| (field.to_owned(), text.to_owned()))
                    .collect(),
            )
            .unwrap();
        retained.add_document(id, fields).unwrap();
        assert_eq!(
            control.memory().used(),
            corpus_bytes(&retained.index.state) + binding_bytes
        );
    }
    retained.add_document(11, []).unwrap();
    let retained = retained.finish().unwrap();
    for field in ["body", "empty", "missing"] {
        let actual = retained.field_stats(field).unwrap();
        let expected = ordinary.field_stats(field).unwrap();
        assert_eq!(actual.total_docs, expected.total_docs);
        assert_eq!(actual.avg_doc_length, expected.avg_doc_length);
        for term in ["a", "β", "discarded"] {
            assert_eq!(actual.doc_freq(field, term), expected.doc_freq(field, term));
        }
        assert_eq!(
            retained.field_doc_count(field).unwrap(),
            ordinary.field_doc_count(field).unwrap()
        );
        assert_eq!(
            retained.total_field_length(field).unwrap(),
            ordinary.total_field_length(field).unwrap()
        );
        for id in [2, 7, 9, 11] {
            assert_eq!(
                retained.indexed_field_metadata(id, field).unwrap(),
                ordinary.indexed_field_metadata(id, field).unwrap()
            );
            for term in ["a", "β", "discarded"] {
                let key = TokenTermKey::from_text(term);
                assert_eq!(
                    retained.get_occurrences(id, field, &key).unwrap(),
                    ordinary.get_occurrences(id, field, &key).unwrap()
                );
                assert_eq!(
                    retained.get_posting_list_key(field, &key).unwrap(),
                    ordinary.get_posting_list_key(field, &key).unwrap()
                );
            }
        }
    }
    assert_eq!(retained.doc_count().unwrap(), 3);
    assert!(Arc::ptr_eq(
        &retained.index_analyzer_revision("body").unwrap(),
        &revision
    ));
    assert!(Arc::ptr_eq(
        &retained.search_analyzer_revision("body").unwrap(),
        &search
    ));
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_document_preserves_existing_postings_counters_and_allowance() {
    let mut saw_rejection = false;
    let mut saw_success = false;
    for available in [0, 1, 32, 256, 1024, 2048, 4096, 8192, 16384, 32768] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut index = builder(&control);
        index.add_document(1, [("body", "prior same")]).unwrap();
        let prior = control.memory().used();
        let blocker = control
            .memory()
            .reserve(control.memory().limit() - prior - available)
            .unwrap();
        let before = control.memory().used();
        match index.add_document(2, [("body", "same same β gamma"), ("other", "delta")]) {
            Ok(()) => {
                saw_success = true;
                assert_eq!(index.index.doc_count().unwrap(), 2);
                assert_eq!(
                    control.memory().used(),
                    corpus_bytes(&index.index.state) + blocker.bytes()
                );
            }
            Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                saw_rejection = true;
                assert_eq!(index.index.doc_count().unwrap(), 1);
                assert_eq!(index.index.total_field_length("body").unwrap(), 2);
                assert_eq!(index.index.doc_freq("body", "same").unwrap(), 1);
                assert_eq!(index.index.get_term_freq(2, "body", "same").unwrap(), 0);
                assert_eq!(control.memory().used(), before);
                assert!(index
                    .index
                    .indexed_field_metadata(2, "other")
                    .unwrap()
                    .is_none());
            }
            Err(error) => panic!("unexpected retained text failure: {error}"),
        }
        assert!(control.memory().peak() <= control.memory().limit());
        drop(blocker);
        let bytes = control.memory().used();
        assert!(index.add_document(1, [("body", "replacement")]).is_err());
        assert_eq!(control.memory().used(), bytes);
        assert_eq!(index.index.get_term_freq(1, "body", "prior").unwrap(), 1);
        drop(index);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(saw_rejection && saw_success);
}

#[test]
fn nested_readers_retain_one_charge_and_reject_mutation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = builder(&control);
    index.add_document(1, [("body", "same same")]).unwrap();
    let bytes = corpus_bytes(&index.index.state)
        + size_of::<MemoryInvertedIndex>()
        + size_of::<MemoryReservation>();
    let mut retained = index.finish().unwrap();
    assert_eq!(control.memory().used(), bytes);
    let first = retained.snapshot().unwrap();
    let second = first.snapshot().unwrap();
    let other = StorageReadControl::with_limit(0);
    let third = second.snapshot_with_control(&other).unwrap();
    assert_eq!(other.memory().used(), 0);
    assert_eq!(control.memory().used(), bytes);
    assert!(retained.clear().is_err());
    assert!(retained.add_document(2, BTreeMap::new()).is_err());
    assert!(retained.writable_snapshot().is_err());
    assert!(retained
        .rebuild_with_analyzer_revision(
            "body",
            uqa_analysis::keyword_analyzer().compile().unwrap(),
            AnalyzerPhase::Both,
            vec![]
        )
        .is_err());
    drop(retained);
    drop(first);
    drop(second);
    assert_eq!(control.memory().used(), bytes);
    assert_eq!(third.get_term_freq(1, "body", "same").unwrap(), 2);
    drop(third);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn direct_owner_recapture_cannot_escape_retention_cancellation_or_read_only_state() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = builder(&control);
    index
        .add_document(1, [("body", "original original")])
        .unwrap();
    let retained = index.finish().unwrap();
    let other = StorageReadControl::with_limit(0);
    let owner: &dyn InvertedIndex = &*retained;
    let mut rebound = owner.snapshot_with_control(&other).unwrap();
    let ordinary = owner.snapshot().unwrap();
    assert_eq!(other.memory().used(), 0);
    assert!(owner.writable_snapshot().is_err());
    assert!(Arc::get_mut(&mut rebound).unwrap().clear().is_err());
    control.cancellation().cancel();
    assert!(matches!(
        owner.doc_count(),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    drop(retained);
    assert!(control.memory().used() > 0);
    assert_eq!(rebound.get_term_freq(1, "body", "original").unwrap(), 2);
    control.cancellation().cancel();
    for view in [&rebound, &ordinary] {
        assert!(matches!(
            view.doc_count(),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            view.snapshot_with_control(&other),
            Err(StorageBackendError::Cancelled(_))
        ));
    }
    drop(rebound);
    drop(ordinary);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn reconstructed_morphology_keeps_lossless_terms_and_graphs_when_enabled() {
    for (tokenizer, descriptor, text) in [
        (
            "nori_tokenizer",
            r#"{"char_filters":[{"type":"html_strip"}],"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"🙂a 가 나"}}"#,
            "<b>🙂a</b>",
        ),
        (
            "kuromoji_tokenizer",
            r#"{"char_filters":[{"type":"html_strip"}],"tokenizer":{"type":"kuromoji_tokenizer","mode":"search","discard_compound_token":false}}"#,
            "<b>関西国際空港に行きました。</b>",
        ),
    ] {
        let config = match serde_json::from_str::<uqa_analysis::Analyzer>(descriptor) {
            Ok(config) => config,
            Err(error) => {
                assert!(error
                    .to_string()
                    .contains(&format!("unknown variant `{tokenizer}`")));
                continue;
            }
        };
        let mut ordinary = MemoryInvertedIndex::new(config.clone());
        ordinary
            .add_document(1, BTreeMap::from([("body".into(), text.into())]))
            .unwrap();
        let control = StorageReadControl::with_limit(8 << 20);
        let mut index =
            RetainedInvertedIndexBuilder::new(&AnalyzerBindings::new(config), &control).unwrap();
        index.add_document(1, [("body", text)]).unwrap();
        assert_eq!(control.memory().used(), corpus_bytes(&index.index.state));
        let retained = index.finish().unwrap();
        let captured = ordinary.snapshot_with_control(&control).unwrap();
        let keys = ordinary.vocabulary_keys("body").unwrap();
        assert!(!keys.is_empty());
        assert_eq!(retained.vocabulary_keys("body").unwrap(), keys);
        let mut long_edge = false;
        for key in &keys {
            let expected = ordinary.get_occurrences(1, "body", key).unwrap();
            long_edge |= expected.iter().any(|edge| edge.position_length > 1);
            assert_eq!(retained.get_occurrences(1, "body", key).unwrap(), expected);
            assert_eq!(captured.get_occurrences(1, "body", key).unwrap(), expected);
        }
        assert!(long_edge);
        if tokenizer == "nori_tokenizer" {
            let raw = TokenTermKey::from_term(&uqa_analysis::TokenTerm::from_utf16(vec![0xd83d]));
            assert!(keys.contains(&raw));
            assert_eq!(retained.get_term_freq_key(1, "body", &raw).unwrap(), 1);
        }
        assert_eq!(
            retained.indexed_field_metadata(1, "body").unwrap(),
            ordinary.indexed_field_metadata(1, "body").unwrap()
        );
        assert_eq!(
            captured.indexed_field_metadata(1, "body").unwrap(),
            ordinary.indexed_field_metadata(1, "body").unwrap()
        );
        drop(captured);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn cancellation_and_shared_owner_allocation_failure_release_the_corpus() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = builder(&control);
    index.add_document(1, [("body", "prior")]).unwrap();
    let prior = control.memory().used();
    let cancellation = control.cancellation().clone();
    let fields = std::iter::once(("body", "candidate")).chain(std::iter::from_fn(move || {
        cancellation.cancel();
        None
    }));
    assert!(matches!(
        index.add_document(2, fields),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), prior);
    assert_eq!(index.index.doc_count().unwrap(), 1);
    assert!(matches!(
        index.finish(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);

    for available in [0, size_of::<MemoryInvertedIndex>()] {
        let memory = MemoryBudget::new(1 << 20);
        let control = StorageReadControl::new(&memory, &uqa_core::CancellationToken::new());
        let mut index = builder(&control);
        index.add_document(1, [("body", "prior")]).unwrap();
        let blocker = memory
            .reserve(memory.limit() - memory.used() - available)
            .unwrap();
        assert!(matches!(
            index.finish(),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(memory.used(), blocker.bytes());
        drop(blocker);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn complete_node_admission_precedes_document_publication() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = builder(&control);
    index.add_document(1, [("body", "same")]).unwrap();
    let prior = control.memory().used();
    let fields = BTreeMap::from([("body", "same new"), ("empty", "")]);
    let staged = stage_selected(
        &index.index,
        2,
        fields.iter().map(|(&name, &text)| (name, text)),
        &control,
    )
    .unwrap();
    let plan = index.plan(2, &staged.fields).unwrap();
    let charge = charge::new_document(&index.index.state, &staged, &plan, &control).unwrap();
    let expected = OwnedMap::<DocId, OwnedMap<FieldName, IndexedFieldMetadata>>::entry_bytes()
        + OwnedMap::<DocId, OwnedSet<PostingKey>>::entry_bytes()
        + 2 * OwnedMap::<DocId, MemoryPosting>::entry_bytes()
        + OwnedMap::<PostingKey, OwnedMap<DocId, MemoryPosting>>::entry_bytes()
        + 2 * OwnedMap::<FieldName, u64>::entry_bytes();
    assert_eq!(charge.new_entries, expected);
    let blocker = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - expected + 1)
        .unwrap();
    assert!(matches!(
        control.memory().reserve(charge.new_entries),
        Err(MemoryError::Limit { .. })
    ));
    assert_eq!(index.index.doc_count().unwrap(), 1);
    assert_eq!(index.index.get_term_freq(1, "body", "same").unwrap(), 1);
    drop(blocker);
    drop(plan);
    drop(staged);
    assert_eq!(control.memory().used(), prior);
    index.add_document(2, fields).unwrap();
    assert_eq!(control.memory().used(), corpus_bytes(&index.index.state));
    assert_eq!(index.index.doc_count().unwrap(), 2);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn staged_field_term_and_plan_nodes_have_complete_leases_before_publication() {
    let control = StorageReadControl::with_limit(1 << 20);
    let index = builder(&control);
    let prior = control.memory().used();
    let staged = stage_selected(
        &index.index,
        1,
        [("body", "same same new"), ("empty", "")],
        &control,
    )
    .unwrap();
    let staged_bytes = staged.fields.allocated_bytes()
        + staged.fields.keys().map(String::capacity).sum::<usize>()
        + staged.terms.allocated_bytes()
        + staged
            .terms
            .iter()
            .map(|(field, term)| field.capacity() + term.allocated_bytes())
            .sum::<usize>()
        + staged.postings.capacity() * size_of::<(PostingKey, MemoryPosting)>()
        + staged
            .postings
            .iter()
            .map(|((field, term), posting)| {
                field.capacity()
                    + term.allocated_bytes()
                    + posting.occurrences.capacity() * size_of::<TokenOccurrence>()
                    + posting.projection.payload.positions.capacity() * size_of::<u32>()
            })
            .sum::<usize>();
    assert_eq!(staged.reserved_bytes(), staged_bytes);
    assert_eq!(control.memory().used(), prior + staged_bytes);
    let entry_only = staged.fields.len() * size_of::<(FieldName, MemoryFieldCounters)>();
    let blocker = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - entry_only)
        .unwrap();
    assert!(matches!(
        index.plan(1, &staged.fields),
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(
        control.memory().used(),
        prior + staged_bytes + blocker.bytes()
    );
    drop(blocker);
    let plan = index.plan(1, &staged.fields).unwrap();
    let plan_bytes = plan.field_counters.allocated_bytes()
        + plan
            .field_counters
            .iter()
            .map(|(field, counters)| field.capacity() + counters.total_key.capacity())
            .sum::<usize>();
    assert_eq!(plan.reserved_bytes(), plan_bytes);
    assert_eq!(control.memory().used(), prior + staged_bytes + plan_bytes);
    drop(plan);
    drop(staged);
    assert_eq!(control.memory().used(), prior);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn borrowed_field_nodes_reject_entry_only_allowance_and_release_on_late_input_cancellation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = builder(&control);
    let prior = control.memory().used();
    let blocker = control
        .memory()
        .reserve(control.memory().limit() - prior - size_of::<(&str, &str)>())
        .unwrap();
    assert!(matches!(
        index.add_document(1, [("body", "")]),
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(control.memory().used(), prior + blocker.bytes());
    assert_eq!(index.index.doc_count().unwrap(), 0);
    drop(blocker);
    let cancellation = control.cancellation().clone();
    let fields = [("body", ""), ("other", "")]
        .into_iter()
        .chain(std::iter::from_fn(move || {
            cancellation.cancel();
            None
        }));
    assert!(matches!(
        index.add_document(1, fields),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), prior);
    control.cancellation().reset();
    let cancellation = control.cancellation().clone();
    let empty = std::iter::from_fn(move || {
        cancellation.cancel();
        None
    });
    assert!(matches!(
        index.add_document(1, empty),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), prior);
    control.cancellation().reset();
    index
        .add_document(1, [("body", "discarded"), ("body", "last")])
        .unwrap();
    assert_eq!(
        index.index.get_term_freq(1, "body", "discarded").unwrap(),
        0
    );
    assert_eq!(index.index.get_term_freq(1, "body", "last").unwrap(), 1);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}
