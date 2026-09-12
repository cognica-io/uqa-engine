//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual memory-provider graph, source metadata, and analyzer generation coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::{
    whitespace_analyzer, Analyzer, AnalyzerResources, TokenLengthPolicy, TokenTerm,
};
use uqa_core::{TokenOccurrence, TokenOffsets};
use uqa_storage::inverted_index::IndexedFieldMetadata;
use uqa_storage::{AnalyzerPhase, InvertedIndex, MemoryInvertedIndex, TokenTermKey};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn config() -> Analyzer {
    serde_json::from_str(
        r#"{
        "tokenizer":{"type":"whitespace"},
        "token_filters":[
            {"type":"stop","language":"","custom_words":["gap"]},
            {"type":"synonym","synonyms":{"a":["a","a"]}}
        ]
    }"#,
    )
    .unwrap()
}

fn offsets(start: u64, end: u64) -> TokenOffsets {
    TokenOffsets {
        start_utf8: start,
        end_utf8: end,
        start_utf16: start,
        end_utf16: end,
    }
}

fn metadata(index: &dyn InvertedIndex, doc: u64) -> IndexedFieldMetadata {
    index.indexed_field_metadata(doc, "body").unwrap().unwrap()
}

#[test]
fn actual_memory_postings_retain_gaps_multiplicity_offsets_and_both_length_policies() {
    for (policy, expected_length) in [
        (TokenLengthPolicy::EmittedTokens, 4),
        (TokenLengthPolicy::DiscountOverlaps, 2),
    ] {
        let revision = AnalyzerResources::default()
            .compile_with_length_policy(&config(), policy)
            .unwrap();
        let mut index = MemoryInvertedIndex::new(whitespace_analyzer());
        index
            .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
            .unwrap();
        index.add_document(7, fields("gap a gap b gap")).unwrap();
        index.add_document(9, fields("gap gap")).unwrap();
        index.add_document(11, fields("")).unwrap();
        let key = TokenTermKey::from_text("a");
        let expected = vec![
            TokenOccurrence {
                position: 1,
                position_length: 1,
                offsets: Some(offsets(4, 5))
            };
            3
        ];
        assert_eq!(index.get_occurrences(7, "body", &key).unwrap(), expected);
        assert_eq!(
            index.get_occurrence_postings("body", &key).unwrap()[0].occurrences,
            expected
        );
        assert_eq!(
            index
                .get_posting_list("body", "a")
                .unwrap()
                .iter()
                .next()
                .unwrap()
                .payload
                .positions,
            [1]
        );
        assert_eq!(index.get_posting_list_key("body", &key).unwrap().len(), 1);
        assert_eq!(index.get_term_freq(7, "body", "a").unwrap(), 3);
        assert_eq!(index.get_term_freq_key(7, "body", &key).unwrap(), 3);
        let mut frequencies = Vec::new();
        index
            .for_each_term_freq("body", "a", &mut |doc, count| {
                frequencies.push((doc, count));
            })
            .unwrap();
        assert_eq!(frequencies, [(7, 3)]);
        for cursor in [
            index.posting_cursor("body", "a").unwrap(),
            index.posting_cursor_key("body", &key).unwrap(),
        ] {
            let score = cursor.current().unwrap();
            assert_eq!(
                (score.doc_id, score.term_freq, score.doc_length),
                (7, 3, expected_length)
            );
        }
        let saved = metadata(&index, 7);
        assert_eq!(
            saved.analyzer_fingerprint,
            revision.descriptor().fingerprint()
        );
        assert_eq!(saved.occurrence_format_version, 2);
        assert_eq!(saved.length_policy, policy);
        assert_eq!(saved.length, expected_length);
        assert_eq!(saved.final_position_increment, 1);
        assert_eq!(saved.final_offsets, offsets(15, 15));
        assert_eq!(metadata(&index, 9).length, 0);
        assert_eq!(metadata(&index, 9).final_position_increment, 2);
        assert_eq!(metadata(&index, 9).final_offsets, offsets(7, 7));
        assert_eq!(metadata(&index, 11).final_offsets, offsets(0, 0));
        assert_eq!(index.field_doc_count("body").unwrap(), 3);
        assert_eq!(index.total_field_length("body").unwrap(), expected_length);
        assert_eq!(index.doc_freq_key("body", &key).unwrap(), 1);
        assert_eq!(index.stats().unwrap().doc_freq("body", "a"), 1);
        assert_eq!(index.vocabulary_terms("body").unwrap(), ["a", "b"]);
        assert!(index
            .indexed_field_metadata(7, "missing")
            .unwrap()
            .is_none());
        assert!(index.get_occurrences(9, "body", &key).unwrap().is_empty());
    }
}

#[test]
fn populated_fields_require_atomic_source_rebuilds_for_a_new_index_revision() {
    let first = config().compile().unwrap();
    let next = whitespace_analyzer().compile().unwrap();
    let mut index = MemoryInvertedIndex::new(whitespace_analyzer());
    index
        .set_field_analyzer_revision("body", first.clone(), AnalyzerPhase::Both)
        .unwrap();
    index.add_document(1, fields("gap a gap")).unwrap();
    let saved = metadata(&index, 1);
    let snapshot = index.snapshot().unwrap();
    for phase in [AnalyzerPhase::Index, AnalyzerPhase::Both] {
        assert!(index
            .set_field_analyzer_revision("body", next.clone(), phase)
            .unwrap_err()
            .contains("atomic source rebuild"));
        assert!(index
            .set_field_analyzer("body", whitespace_analyzer(), phase)
            .unwrap_err()
            .contains("atomic source rebuild"));
    }
    assert!(index
        .remove_field_analyzers("body")
        .unwrap_err()
        .contains("atomic source rebuild"));
    assert_eq!(metadata(&index, 1), saved);
    assert!(Arc::ptr_eq(
        &index.index_analyzer_revision("body").unwrap(),
        &first
    ));
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("body").unwrap(),
        &first
    ));
    index
        .set_field_analyzer_revision("body", next.clone(), AnalyzerPhase::Search)
        .unwrap();
    assert_eq!(metadata(&index, 1), saved);
    index
        .rebuild_with_analyzer_revision(
            "body",
            next.clone(),
            AnalyzerPhase::Index,
            vec![(1, fields("gap a gap"))],
        )
        .unwrap();
    assert_eq!(
        metadata(&index, 1).analyzer_fingerprint,
        next.descriptor().fingerprint()
    );
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 1);
    assert_eq!(index.get_term_freq(1, "body", "gap").unwrap(), 2);
    assert_eq!(metadata(snapshot.as_ref(), 1), saved);
    assert_eq!(snapshot.get_term_freq(1, "body", "a").unwrap(), 3);
    assert!(Arc::ptr_eq(
        &snapshot.search_analyzer_revision("body").unwrap(),
        &first
    ));
    index.remove_document(1).unwrap();
    index.remove_field_analyzers("body").unwrap();
    index.add_document(1, fields("gap a gap")).unwrap();
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 1);
}

#[test]
fn tokenless_fields_also_retain_their_revision_until_a_source_rebuild() {
    let mut index = MemoryInvertedIndex::new(config());
    index.add_document(1, fields("gap gap")).unwrap();
    assert_eq!(index.posting_count(None).unwrap(), 0);
    let next = whitespace_analyzer().compile().unwrap();
    assert!(index
        .set_field_analyzer_revision("body", next.clone(), AnalyzerPhase::Index)
        .is_err());
    index
        .rebuild_with_analyzer_revision(
            "body",
            next,
            AnalyzerPhase::Index,
            vec![(1, fields("gap gap"))],
        )
        .unwrap();
    assert_eq!(index.get_term_freq(1, "body", "gap").unwrap(), 2);
    assert_eq!(metadata(&index, 1).final_position_increment, 0);
}

#[test]
fn replacement_removal_and_repeated_batch_ids_publish_complete_graph_state() {
    let mut index = MemoryInvertedIndex::new(config());
    index.add_document(1, fields("a")).unwrap();
    let snapshot = index.writable_snapshot().unwrap();
    index
        .try_add_documents(vec![
            (1, fields("b b")),
            (2, fields("a a")),
            (1, fields("gap")),
            (2, BTreeMap::new()),
            (u64::MAX, fields("a")),
        ])
        .unwrap();
    assert_eq!(index.doc_count().unwrap(), 2);
    assert_eq!(index.posting_count(None).unwrap(), 1);
    assert_eq!(index.get_doc_length(1, "body").unwrap(), 0);
    assert_eq!(index.get_doc_length(u64::MAX, "body").unwrap(), 3);
    assert_eq!(index.total_field_length("body").unwrap(), 3);
    assert!(index.indexed_field_metadata(2, "body").unwrap().is_none());
    assert_eq!(
        index
            .get_occurrence_postings("body", &TokenTermKey::from_text("a"))
            .unwrap()[0]
            .doc_id,
        u64::MAX
    );
    let mut cursor = index.posting_cursor("body", "a").unwrap();
    cursor.advance_to(u64::MAX).unwrap();
    assert_eq!(cursor.current().unwrap().doc_id, u64::MAX);
    assert_eq!(snapshot.get_term_freq(1, "body", "a").unwrap(), 3);
    assert_eq!(metadata(snapshot.as_ref(), 1).final_offsets, offsets(1, 1));
    index.remove_document(u64::MAX).unwrap();
    assert_eq!(index.total_field_length("body").unwrap(), 0);
    assert!(index.vocabulary_keys("body").unwrap().is_empty());
    index.clear().unwrap();
    assert!(index.indexed_field_metadata(1, "body").unwrap().is_none());
    assert_eq!(index.doc_count().unwrap(), 0);
    assert_eq!(index.doc_length_count(None).unwrap(), 0);
}

#[test]
fn failed_point_batch_and_revision_rebuilds_preserve_original_graph_metadata() {
    let invalid: Analyzer =
        serde_json::from_str(r#"{"tokenizer":{"type":"n_gram","min_gram":0,"max_gram":1}}"#)
            .unwrap();
    let mut index = MemoryInvertedIndex::new(invalid);
    index
        .set_field_analyzer("body", config(), AnalyzerPhase::Both)
        .unwrap();
    index.add_document(1, fields("gap a gap")).unwrap();
    let saved = metadata(&index, 1);
    let first = index.index_analyzer_revision("body").unwrap();
    let malformed = BTreeMap::from([
        ("body".into(), "b".into()),
        ("unresolved".into(), "value".into()),
    ]);
    assert!(index.add_document(1, malformed.clone()).is_err());
    assert!(index
        .try_add_documents(vec![(1, fields("b")), (2, malformed.clone())])
        .is_err());
    assert!(index
        .rebuild_with_analyzer_revision(
            "body",
            whitespace_analyzer().compile().unwrap(),
            AnalyzerPhase::Both,
            vec![(1, fields("b")), (2, malformed)]
        )
        .is_err());
    assert_eq!(metadata(&index, 1), saved);
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.vocabulary_terms("body").unwrap(), ["a"]);
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 3);
    assert_eq!(
        index
            .get_occurrences(1, "body", &TokenTermKey::from_text("a"))
            .unwrap()
            .len(),
        3
    );
    assert!(Arc::ptr_eq(
        &index.index_analyzer_revision("body").unwrap(),
        &first
    ));
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("body").unwrap(),
        &first
    ));
}

#[test]
fn korean_memory_storage_retains_unpaired_identity_long_edges_and_original_source() {
    let config = serde_json::from_str::<Analyzer>(
        r#"{
        "char_filters":[{"type":"html_strip"}],
        "tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"🙂a 가 나"},
        "token_filters":[]
    }"#,
    );
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return;
        }
    };
    let mut index = MemoryInvertedIndex::new(config);
    let source = "<b>🙂a</b>";
    index.add_document(1, fields(source)).unwrap();
    let key = TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd83d]));
    let occurrences = index.get_occurrences(1, "body", &key).unwrap();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(
        occurrences[0].offsets,
        Some(TokenOffsets {
            start_utf8: 3,
            end_utf8: 7,
            start_utf16: 4,
            end_utf16: 5
        })
    );
    assert_eq!(index.get_term_freq_key(1, "body", &key).unwrap(), 1);
    assert_eq!(index.doc_freq_key("body", &key).unwrap(), 1);
    assert_eq!(index.get_posting_list_key("body", &key).unwrap().len(), 1);
    assert_eq!(
        index
            .posting_cursor_key("body", &key)
            .unwrap()
            .current()
            .unwrap()
            .term_freq,
        1
    );
    assert!(index.vocabulary_keys("body").unwrap().contains(&key));
    assert!(index.vocabulary_terms("body").is_err());
    assert_eq!(index.stats().unwrap().doc_freq_utf16("body", &[0xd83d]), 1);
    assert_eq!(index.stats().unwrap().doc_freq("body", "�"), 0);
    let compound = index
        .get_occurrences(1, "body", &TokenTermKey::from_text("🙂a"))
        .unwrap();
    assert!(compound.iter().any(|edge| edge.position_length == 2));
    let saved = metadata(&index, 1);
    assert_eq!(saved.length, 2);
    assert_eq!(saved.length_policy, TokenLengthPolicy::DiscountOverlaps);
    assert_eq!(saved.final_offsets.end_utf8, source.len() as u64);
    assert_eq!(
        saved.final_offsets.end_utf16,
        source.encode_utf16().count() as u64
    );
    let snapshot = index.snapshot().unwrap();
    index.add_document(1, fields("한국")).unwrap();
    assert_eq!(index.doc_freq_key("body", &key).unwrap(), 0);
    assert_eq!(index.stats().unwrap().doc_freq_utf16("body", &[0xd83d]), 0);
    assert_eq!(
        snapshot.get_occurrences(1, "body", &key).unwrap(),
        occurrences
    );
    assert_eq!(metadata(snapshot.as_ref(), 1), saved);
    index.remove_document(1).unwrap();
    assert_eq!(index.doc_count().unwrap(), 0);
    assert_eq!(index.posting_count(None).unwrap(), 0);
    assert_eq!(index.term_count(None).unwrap(), 0);
}
