//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Differential user-rule coverage, including raw UTF-16 morpheme boundaries.

use serde_json::{json, Value};
use uqa_analysis::nori::{DictionaryError, UserDictionary, UserDictionaryLimits};

use super::nori_resources::model;

#[test]
fn user_dictionary_matches_pinned_docker_compilation_and_prefixes() {
    for line in include_str!("../../../../tests/parity/nori/user_expected.jsonl").lines() {
        let expected: Value = serde_json::from_str(line).unwrap();
        let id = expected["id"].as_str().unwrap();
        let rules = expected["rules"].as_str().unwrap();
        let result = UserDictionary::compile(rules, model(), UserDictionaryLimits::default());
        if expected.get("error").is_some() {
            assert!(result.is_err(), "{id}");
            continue;
        }
        let dictionary = result.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(
            dictionary.is_none(),
            expected["empty"].as_bool().unwrap(),
            "{id}"
        );
        let mut matches = Vec::new();
        if let Some(dictionary) = dictionary {
            assert_eq!(dictionary.source(), rules);
            assert_eq!(dictionary.model_id(), model().id());
            let query: Vec<_> = expected["query"].as_str().unwrap().encode_utf16().collect();
            for start in 0..query.len() {
                for (length, word) in dictionary.prefixes(&query[start..]) {
                    let entry = dictionary.entry(word).unwrap();
                    let parts = entry.segment_lengths().map(|lengths| {
                        let mut offset = start;
                        lengths
                            .iter()
                            .map(|length| {
                                let units = &query[offset..offset + length];
                                offset += length;
                                json!({"surface_utf16": units, "pos": entry.pos()})
                            })
                            .collect::<Vec<_>>()
                    });
                    matches.push(json!({"start": start, "end": start + length, "id": word,
                        "left": entry.left_context(), "right": entry.right_context(), "cost": entry.cost(),
                        "pos_type": entry.pos_type(), "left_pos": entry.pos(), "right_pos": entry.pos(),
                        "reading": null, "morphemes": parts}));
                }
            }
        }
        assert_eq!(Value::Array(matches), expected["matches"], "{id}");
    }
}

#[test]
fn user_dictionary_limits_preserve_empty_and_duplicate_semantics() {
    let limits = UserDictionaryLimits::default();
    let source = "한국\n한국\n";
    assert_eq!(
        UserDictionary::compile(source, model(), limits)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    for bounded in [
        UserDictionaryLimits {
            max_bytes: 1,
            ..limits
        },
        UserDictionaryLimits {
            max_entries: 1,
            ..limits
        },
        UserDictionaryLimits {
            max_surface_utf16: 1,
            ..limits
        },
    ] {
        assert!(matches!(
            UserDictionary::compile(source, model(), bounded),
            Err(DictionaryError::Limit { .. })
        ));
    }
    assert!(UserDictionary::compile(
        "",
        model(),
        UserDictionaryLimits {
            max_bytes: 0,
            max_entries: 0,
            max_surface_utf16: 0
        }
    )
    .unwrap()
    .is_none());
}
