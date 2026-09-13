//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources, TokenLengthPolicy, TokenTerm};
use uqa_core::{TokenOccurrence, TokenOffsets};
use uqa_storage::clustered_postings::{
    decode_all_scores, decode_cluster, decode_occurrence_cluster, decode_term_keys, encode_cluster,
    encode_occurrence_cluster, encode_term_keys, encode_terms, ClusterPosting,
    ClusteredPostingCursor, EncodedScoreCluster, OccurrencePosting,
};
use uqa_storage::inverted_index::analyze_index_field;
use uqa_storage::{MaterializedPostingCursor, PostingCursor, TokenTermKey};

#[path = "occurrences/corruption.rs"]
mod corruption;

fn edge(position: u32, length: u32, offsets: Option<TokenOffsets>) -> TokenOccurrence {
    TokenOccurrence {
        position,
        position_length: length,
        offsets,
    }
}

fn span(start_utf8: u64, end_utf8: u64, start_utf16: u64, end_utf16: u64) -> TokenOffsets {
    TokenOffsets {
        start_utf8,
        end_utf8,
        start_utf16,
        end_utf16,
    }
}

#[test]
fn graph_round_trip_retains_multiplicity_lengths_and_exact_source_coordinates() {
    let entries = vec![
        OccurrencePosting {
            doc_id: 3,
            doc_length: 2,
            occurrences: vec![
                edge(0, 3, Some(span(3, 7, 4, 5))),
                edge(0, 1, Some(span(3, 7, 4, 5))),
                edge(0, 1, Some(span(3, 7, 4, 5))),
                edge(4, 1, Some(span(0, 0, 0, 0))),
                edge(4, 2, None),
            ],
        },
        OccurrencePosting {
            doc_id: 65_535,
            doc_length: 1,
            occurrences: vec![edge(
                u32::MAX - 1,
                1,
                Some(span(u64::MAX, u64::MAX, u64::MAX, u64::MAX)),
            )],
        },
    ];
    let (scores, positions) = encode_occurrence_cluster(&entries).unwrap();
    assert_eq!(
        decode_occurrence_cluster(0, &scores, &positions).unwrap(),
        entries
    );
    assert_eq!(entries[0].positions(), [0, 4]);
    assert_eq!(
        decode_all_scores(0, &scores).unwrap(),
        entries
            .iter()
            .map(OccurrencePosting::score)
            .collect::<Vec<_>>()
    );
    assert_eq!(entries[0].score().term_freq, 5);
    assert_eq!(entries[0].score().doc_length, 2);
    assert!(decode_cluster(0, &scores, &positions).is_err());
    // Score cursors receive no positional data and must retain overlap-discounted lengths.
    let mut cursor = ClusteredPostingCursor::new(vec![EncodedScoreCluster {
        cluster_id: 0,
        bytes: scores,
    }])
    .unwrap();
    assert_eq!(cursor.current(), Some(entries[0].score()));
    assert_eq!(cursor.advance_to(50_000).unwrap(), Some(entries[1].score()));
    assert_eq!(cursor.advance().unwrap(), None);
    let cursor =
        MaterializedPostingCursor::new(entries.iter().map(OccurrencePosting::score).collect())
            .unwrap();
    assert_eq!(cursor.current(), Some(entries[0].score()));
}

#[test]
fn term_keys_preserve_all_units_and_reject_alternate_scalar_encodings() {
    let mut keys = BTreeMap::new();
    for unit in 0..=u16::MAX {
        let term = TokenTerm::from_utf16(vec![unit]);
        let key = TokenTermKey::from_term(&term);
        assert_eq!(
            TokenTermKey::from_bytes(key.as_bytes().to_vec()).unwrap(),
            key
        );
        assert_eq!(key.to_term(), term);
        assert!(keys.insert(key, unit).is_none());
    }
    for text in ["", "\0", "🙂", "x\u{fffd}", "한국", "\u{ffff}"] {
        let units = TokenTerm::from_utf16(text.encode_utf16().collect());
        assert_eq!(
            TokenTermKey::from_term(&units),
            TokenTermKey::from_text(text)
        );
    }
    for invalid in [
        vec![],
        vec![2],
        vec![0, 0xff],
        vec![1],
        vec![1, 0],
        vec![1, 0, 97],
        vec![1, 0xd8, 0x3d, 0xde, 0x42],
    ] {
        assert!(TokenTermKey::from_bytes(invalid).is_err());
    }
    let terms: Vec<_> = keys.into_keys().collect();
    let encoded = encode_term_keys(&terms).unwrap();
    assert_eq!(decode_term_keys(&encoded).unwrap(), terms);
    assert_eq!(
        decode_term_keys(&encode_term_keys(&[]).unwrap()).unwrap(),
        []
    );
    assert!(decode_term_keys(&encode_terms(&["old".into()]).unwrap()).is_err());
}

#[test]
fn immutable_field_staging_keeps_gaps_duplicate_alternatives_and_declared_length() {
    let config: Analyzer = serde_json::from_str(
        r#"{
        "tokenizer":{"type":"whitespace"},
        "token_filters":[
            {"type":"stop","language":"","custom_words":["gap"]},
            {"type":"synonym","synonyms":{"a":["same","same"]}}
        ]
    }"#,
    )
    .unwrap();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let emitted = resources
        .compile_with_length_policy(&config, TokenLengthPolicy::EmittedTokens)
        .unwrap();
    let discounted = resources
        .compile_with_length_policy(&config, TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    let first = analyze_index_field(&emitted, "gap a gap b gap").unwrap();
    let second = analyze_index_field(&discounted, "gap a gap b gap").unwrap();
    assert_eq!(first.terms, second.terms);
    assert_eq!(first.length, 4);
    assert_eq!(second.length, 2);
    assert_eq!(second.final_position_increment, 1);
    assert_eq!(second.final_offsets, span(15, 15, 15, 15));
    assert_eq!(
        second.terms[&TokenTermKey::from_text("same")],
        [edge(1, 1, Some(span(4, 5, 4, 5))); 2]
    );
    assert_eq!(
        second.terms[&TokenTermKey::from_text("b")],
        [edge(3, 1, Some(span(10, 11, 10, 11)))]
    );
    for (term, occurrences) in second.terms {
        let key_bytes = term.into_bytes();
        assert!(TokenTermKey::from_bytes(key_bytes)
            .unwrap()
            .to_term()
            .as_str()
            .is_some());
        let entry = OccurrencePosting {
            doc_id: 1,
            doc_length: second.length,
            occurrences,
        };
        let (scores, positions) = encode_occurrence_cluster(std::slice::from_ref(&entry)).unwrap();
        assert_eq!(
            decode_occurrence_cluster(0, &scores, &positions).unwrap(),
            [entry]
        );
    }
}

#[test]
fn korean_field_staging_preserves_unpaired_terms_and_mixed_compound_edges() {
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
    let compiled = config.compile().unwrap();
    let source = "<b>🙂a</b>";
    let staged = analyze_index_field(&compiled, source).unwrap();
    assert_eq!(staged.length, 2);
    assert_eq!(staged.terms.values().map(Vec::len).sum::<usize>(), 3);
    let raw_key = TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd83d]));
    assert!(raw_key.to_term().as_str().is_none());
    assert_eq!(staged.terms[&raw_key][0].offsets, Some(span(3, 7, 4, 5)));
    assert!(staged.terms[&TokenTermKey::from_text("🙂a")]
        .iter()
        .any(|item| item.position_length == 2));
    for occurrences in staged.terms.into_values() {
        let entry = OccurrencePosting {
            doc_id: 9,
            doc_length: staged.length,
            occurrences,
        };
        let (scores, positions) = encode_occurrence_cluster(std::slice::from_ref(&entry)).unwrap();
        assert_eq!(
            decode_occurrence_cluster(0, &scores, &positions).unwrap(),
            [entry]
        );
    }
}

#[test]
fn legacy_clusters_stay_readable_without_fabricating_graphs() {
    let legacy = vec![ClusterPosting {
        doc_id: 0,
        term_freq: 2,
        doc_length: 3,
        positions: vec![0, 2],
    }];
    let (scores, positions) = encode_cluster(&legacy).unwrap();
    assert_eq!(decode_cluster(0, &scores, &positions).unwrap(), legacy);
    assert!(decode_occurrence_cluster(0, &scores, &positions)
        .unwrap_err()
        .to_string()
        .contains("source rebuild"));
    let cursor = ClusteredPostingCursor::new(vec![EncodedScoreCluster {
        cluster_id: 0,
        bytes: scores,
    }])
    .unwrap();
    assert_eq!(cursor.current().unwrap().term_freq, 2);
}

#[test]
fn score_cursors_cross_legacy_and_graph_blocks_without_decoding_positions() {
    let (legacy, _) = encode_cluster(&[ClusterPosting {
        doc_id: 65_535,
        term_freq: 1,
        doc_length: 2,
        positions: vec![0],
    }])
    .unwrap();
    let entries: Vec<_> = (0..260)
        .map(|offset| OccurrencePosting {
            doc_id: 65_536 + offset * 2,
            doc_length: 1,
            occurrences: vec![edge(0, 1, None); 3],
        })
        .collect();
    let (graph, _) = encode_occurrence_cluster(&entries).unwrap();
    let mut cursor = ClusteredPostingCursor::new(vec![
        EncodedScoreCluster {
            cluster_id: 0,
            bytes: legacy,
        },
        EncodedScoreCluster {
            cluster_id: 1,
            bytes: graph,
        },
    ])
    .unwrap();
    assert_eq!(cursor.doc_freq(), 261);
    assert_eq!(
        cursor.advance_to(65_536 + 400).unwrap(),
        Some(entries[200].score())
    );
    assert_eq!(cursor.ordinal(), 201);
    let mut cloned = cursor.boxed_clone();
    assert_eq!(cloned.advance().unwrap(), Some(entries[201].score()));
    assert_eq!(cursor.current(), Some(entries[200].score()));
    assert_eq!(cursor.advance_to(65_536 + 520).unwrap(), None);
    assert_eq!(cursor.ordinal(), 261);
}
