//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::fmt::Write;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("user preparation retains original source")
    }
}

fn input(
    source: Arc<str>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Option<Arc<UserDictionary>>, DictionaryError> {
    let dictionary = model().clone();
    ImageDecoder::new(memory, token, |mut channel| async move {
        UserDictionary::compile_with(
            &source,
            &dictionary,
            UserDictionaryLimits::default(),
            &mut channel.input(),
        )
        .await
    })
    .unwrap()
}

#[test]
fn retained_japanese_user_preparation_matches_pinned_csv_and_morphology() {
    for line in include_str!("../../../../../../tests/parity/kuromoji/user_expected.jsonl").lines()
    {
        let expected: Value = serde_json::from_str(line).unwrap();
        let id = expected["id"].as_str().unwrap();
        let source: Arc<str> = expected["rules"].as_str().unwrap().into();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut decoder = input(source.clone(), &memory, &token);
        let result = loop {
            match decoder.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 }) {
                Ok(Some(value)) => break Ok(value),
                Ok(None) => (),
                Err(error) => break Err(error),
            }
        };
        assert_eq!(memory.used(), 0);
        assert_eq!(Arc::strong_count(&source), 1);
        if expected["error_stage"] == "compile" {
            assert!(
                matches!(result, Err(DictionaryError::Invalid { .. })),
                "{id}"
            );
            continue;
        }
        let dictionary = result.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(
            dictionary.is_none(),
            expected["empty"].as_bool().unwrap(),
            "{id}"
        );
        if let Some(dictionary) = dictionary {
            assert_eq!(dictionary.source(), source.as_ref());
            let query: Vec<_> = expected["query"].as_str().unwrap().encode_utf16().collect();
            let actual = outputs(&dictionary, &query);
            if expected["error_stage"] == "prefix" {
                assert!(
                    matches!(actual, Err(DictionaryError::Invalid { .. })),
                    "{id}"
                );
            } else {
                let (prefixes, matches) = actual.unwrap();
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
}

#[test]
fn retained_japanese_user_preparation_retires_wide_rules_during_each_construction_pass() {
    let prefix = "尾".repeat(128);
    let mut rules = String::new();
    for i in (0..128).rev() {
        let name = format!("{prefix}{i:04}");
        writeln!(rules, "{name},{name},ヨミ,名詞").unwrap();
    }
    let source: Arc<str> = rules.into();
    let mut total = 0;
    for (fraction, action) in std::iter::once((usize::MAX, 0))
        .chain((0..5).flat_map(|fraction| (0..3).map(move |action| (fraction, action))))
    {
        let stop = if fraction == usize::MAX {
            usize::MAX
        } else {
            total * fraction / 5
        };
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut decoder = input(source.clone(), &memory, &token);
        let mut calls = 0;
        while calls < stop {
            if let Some(dictionary) = decoder
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                assert_eq!(stop, usize::MAX);
                let dictionary = dictionary.unwrap();
                assert_eq!(dictionary.source(), source.as_ref());
                assert_eq!(dictionary.len(), 128);
                for rank in 0..128 {
                    assert_eq!(dictionary.lookup(&format!("{prefix}{rank:04}")), Some(rank));
                }
                total = calls;
                break;
            }
            calls += 1;
            assert!(calls < 100_000);
        }
        if stop != usize::MAX && action != 0 {
            if action == 1 {
                token.cancel();
            }
            let steps = usize::from(action == 1);
            assert!(decoder
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                .is_err());
            assert_eq!(memory.used(), 0);
            assert_eq!(Arc::strong_count(&source), 1);
        }
        drop(decoder);
        assert_eq!(memory.used(), 0);
        assert_eq!(Arc::strong_count(&source), 1);
    }
    assert!(total > 1024);
}
