//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::AnalysisError;

#[test]
fn bounded_comparisons_preserve_order_across_unicode_and_chunk_boundaries() {
    let mut values: Vec<String> = ["", "a", "a\0", "é", "韓", "🙂", "z"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for length in [1023, 1024, 1025, 4095, 4096] {
        for suffix in ["", "a", "z", "é", "韓", "🙂"] {
            values.push(format!("{}{suffix}", "a".repeat(length)));
        }
    }
    for left in &values {
        for right in &values {
            assert_eq!(
                compare(left, right, &mut || Ok(())).unwrap(),
                left.cmp(right)
            );
        }
    }
    let map: BTreeMap<String, Vec<String>> = values
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
        .map(|(_, value)| (value.clone(), vec![value.clone(), value.clone()]))
        .collect();
    let words: Vec<_> = map.keys().map(|key| Cow::Borrowed(key.as_str())).collect();
    for prepared in [
        PreparedSynonyms::borrowed(&map),
        PreparedSynonyms::owned(map.clone()),
        PreparedSynonyms::borrowed(&map).into_owned(),
    ] {
        for value in &values {
            assert_eq!(
                prepared.get(value, &mut || Ok(())).unwrap(),
                map.get(value).map(Vec::as_slice)
            );
            assert_eq!(
                find_word(&words, value, &mut || Ok(())).unwrap(),
                map.contains_key(value)
            );
        }
    }
}

#[test]
fn long_matching_keys_can_cancel_inside_comparison_and_reuse_prepared_tables() {
    let key = "韓".repeat(8192);
    let map = [(key.clone(), vec!["UQA".into()])].into();
    let prepared = PreparedSynonyms::borrowed(&map);
    let mut calls = 0;
    assert!(prepared
        .get(&key, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap()
        .is_some());
    assert!(calls >= key.len() / 1024);
    for stop in 1..=calls {
        let mut count = 0;
        let result = prepared.get(&key, &mut || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
    }
    assert_eq!(
        prepared.get(&key, &mut || Ok(())).unwrap(),
        map.get(&key).map(Vec::as_slice)
    );
}
