//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use super::{UserDictionary, UserDictionaryLimits, UserWord};
use crate::kuromoji::{DictionaryError, DictionaryLimits, DictionaryResult, KuromojiDictionary};

fn model() -> &'static Arc<KuromojiDictionary> {
    static MODEL: OnceLock<Arc<KuromojiDictionary>> = OnceLock::new();
    MODEL.get_or_init(|| {
        KuromojiDictionary::from_bytes(uqa_kuromoji_data::BUNDLE, DictionaryLimits::default())
            .unwrap()
    })
}

fn word(value: UserWord<'_>) -> DictionaryResult<Value> {
    let units = |text: &str| text.encode_utf16().collect::<Vec<_>>();
    Ok(
        json!({"id":value.id(),"left":value.left_context(),"right":value.right_context(),
        "cost":value.cost(),"reading":units(value.reading()?),"pos":units(value.part_of_speech()?),
        "base_form":value.base_form(),"pronunciation":value.pronunciation(),
        "inflection_type":value.inflection_type(),"inflection_form":value.inflection_form()}),
    )
}

fn outputs(
    dictionary: &UserDictionary,
    query: &[u16],
) -> DictionaryResult<(Vec<Value>, Vec<Value>)> {
    let mut prefixes = Vec::new();
    for start in 0..query.len() {
        for (length, phrase) in dictionary.prefixes(&query[start..]) {
            let entry = dictionary.entry(phrase).unwrap();
            let words = (0..entry.segment_lengths().len())
                .map(|index| word(dictionary.word(entry.word_base() + index as u32).unwrap()))
                .collect::<DictionaryResult<Vec<_>>>()?;
            prefixes.push(json!({"start":start,"end":start+length,"id":phrase,
                "word_base":entry.word_base(),"lengths":entry.segment_lengths(),"words":words}));
        }
    }
    let matches = dictionary
        .matches(query)
        .map(|matched| {
            Ok(json!({"start":matched.start,"length":matched.length,
            "word":word(dictionary.word(matched.word_id).unwrap())?}))
        })
        .collect::<DictionaryResult<Vec<_>>>()?;
    Ok((prefixes, matches))
}

#[test]
fn japanese_user_rules_match_pinned_compilation_prefixes_and_longest_lookup() {
    for line in include_str!("../../../../../tests/parity/kuromoji/user_expected.jsonl").lines() {
        let expected: Value = serde_json::from_str(line).unwrap();
        let id = expected["id"].as_str().unwrap();
        let source = expected["rules"].as_str().unwrap();
        let result = UserDictionary::compile(source, model(), UserDictionaryLimits::default());
        if expected["error_stage"] == "compile" {
            assert!(
                matches!(result, Err(DictionaryError::Invalid { .. })),
                "{id}: {result:?}"
            );
            continue;
        }
        let dictionary = result.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(
            dictionary.is_none(),
            expected["empty"].as_bool().unwrap(),
            "{id}"
        );
        let Some(dictionary) = dictionary else {
            assert_eq!(expected["prefixes"], json!([]), "{id}");
            assert_eq!(expected["matches"], json!([]), "{id}");
            continue;
        };
        assert_eq!(dictionary.model_id(), model().id());
        assert_eq!(dictionary.source(), source);
        let query: Vec<_> = expected["query"].as_str().unwrap().encode_utf16().collect();
        let actual = outputs(&dictionary, &query);
        if expected["error_stage"] == "prefix" {
            assert!(
                matches!(actual, Err(DictionaryError::Invalid { .. })),
                "{id}"
            );
        } else {
            assert!(
                expected.get("error").is_none(),
                "unhandled reference error: {id}"
            );
            let (prefixes, matches) = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
            assert_eq!(
                prefixes,
                expected["prefixes"].as_array().unwrap().as_slice(),
                "{id}"
            );
            assert_eq!(
                matches,
                expected["matches"].as_array().unwrap().as_slice(),
                "{id}"
            );
        }
    }
}

#[test]
fn user_preparation_limits_preserve_retained_models_and_empty_sources() {
    let limits = UserDictionaryLimits::default();
    let source = "東京,東京,トウキョウ,名詞\n大学,大学,ダイガク,名詞";
    let retained = UserDictionary::compile(source, model(), limits)
        .unwrap()
        .unwrap();
    for limits in [
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
            UserDictionary::compile(source, model(), limits),
            Err(DictionaryError::Limit { .. })
        ));
        assert_eq!(retained.source(), source);
        assert_eq!(retained.lookup("東京"), Some(1));
    }
    assert!(UserDictionary::compile(
        "",
        model(),
        UserDictionaryLimits {
            max_bytes: 0,
            max_entries: 0,
            max_surface_utf16: 0,
        }
    )
    .unwrap()
    .is_none());
    assert!(retained.word(99_999_999).is_none());
    assert!(retained.word(u32::MAX).is_none());
    assert!(retained.entry(u32::MAX).is_none());
}

#[test]
fn japanese_user_contexts_require_the_selected_models_matrix() {
    let small = KuromojiDictionary::from_bytes(
        &crate::kuromoji::tests::fixtures::bundle(),
        DictionaryLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        UserDictionary::compile(
            "東京,東京,トウキョウ,名詞",
            &small,
            UserDictionaryLimits::default()
        ),
        Err(DictionaryError::Invalid {
            reason: "model cannot address fixed user contexts",
            ..
        })
    ));
}
