//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{AnalysisError, TokenTerm};
use uqa_core::memory::{MemoryBudget, MemoryError};

#[test]
fn short_words_pass_through() {
    assert_eq!(stem("a"), "a");
    assert_eq!(stem("by"), "by");
}

#[test]
fn known_examples() {
    assert_eq!(stem("caresses"), "caress");
    assert_eq!(stem("ponies"), "poni");
    assert_eq!(stem("ties"), "ti");
    assert_eq!(stem("caress"), "caress");
    assert_eq!(stem("cats"), "cat");
    assert_eq!(stem("feed"), "feed");
    assert_eq!(stem("agreed"), "agre");
    assert_eq!(stem("conflated"), "conflat");
    assert_eq!(stem("troubled"), "troubl");
    assert_eq!(stem("happy"), "happi");
    assert_eq!(stem("relational"), "relat");
    assert_eq!(stem("conditional"), "condit");
    assert_eq!(stem("rational"), "ration");
    assert_eq!(stem("triplicate"), "triplic");
    assert_eq!(stem("formative"), "form");
    assert_eq!(stem("electrical"), "electr");
    assert_eq!(stem("hopeful"), "hope");
    assert_eq!(stem("goodness"), "good");
    assert_eq!(stem("revival"), "reviv");
    assert_eq!(stem("homologous"), "homolog");
    assert_eq!(stem("controll"), "control");
}

#[test]
fn exhaustive_lossless_words_preserve_the_porter_suffix_contract() {
    use sha2::{Digest, Sha256};
    let alphabet = [0x61u16, 0x62, 0x65, 0x79, 0xd800, 0xdc00];
    let suffixes = [
        "s", "sses", "ies", "ss", "eed", "ed", "ing", "y", "ational", "tional", "enci", "anci",
        "izer", "abli", "alli", "entli", "eli", "ousli", "ization", "ation", "ator", "alism",
        "iveness", "fulness", "ousness", "aliti", "iviti", "biliti", "icate", "ative", "alize",
        "iciti", "ical", "ful", "ness", "al", "ance", "ence", "er", "ic", "able", "ible", "ant",
        "ement", "ment", "ent", "ion", "ou", "ism", "ate", "iti", "ous", "ive", "ize", "e", "ll",
    ];
    let mut digest = Sha256::new();
    let mut count = 0;
    for length in 0..=6u32 {
        for mut number in 0..alphabet.len().pow(length) {
            let mut input = Vec::new();
            for _ in 0..length {
                input.push(alphabet[number % alphabet.len()]);
                number /= alphabet.len();
            }
            for suffix in
                std::iter::once("").chain(suffixes.iter().copied().filter(|_| length <= 3))
            {
                let mut word = input.clone();
                word.extend(suffix.encode_utf16());
                let result = stem_utf16(&word);
                digest.update((result.len() as u64).to_le_bytes());
                for unit in result {
                    digest.update(unit.to_le_bytes());
                }
                count += 1;
            }
        }
    }
    assert_eq!(count, 70_491);
    // Complete output captured from the existing Porter 1980 contract, including raw surrogates.
    assert_eq!(
        format!("{:x}", digest.finalize()),
        "4ece881a8fb46369000ed733103f49b9a37cfc22a84de2f87a8b0afe0c87130f"
    );
}

#[test]
fn long_y_runs_use_bounded_stack_and_preserve_all_but_the_final_y() {
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let length = 100_000;
            let input = "y".repeat(length);
            let budget = MemoryBudget::new(1 << 20);
            let mut polls = 0;
            let result = stem_budgeted(&input, &budget, || {
                polls += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(result.len(), length);
            assert!(result[..length - 1]
                .bytes()
                .all(|character| character == b'y'));
            assert!(result.ends_with('i'));
            assert!(polls >= 3 * length / 1024 && polls < length / 32);
            assert_eq!(budget.used(), result.reserved_bytes());
            drop(result);
            assert_eq!(budget.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn stemmer_reservations_cover_live_scratch_and_scalar_or_raw_output() {
    for input in [
        TokenTerm::from("relational"),
        TokenTerm::from("한🙂relational"),
        TokenTerm::from_utf16([vec![0xd800], "relational".encode_utf16().collect()].concat()),
    ] {
        let baseline = MemoryBudget::new(1 << 20);
        let expected = stem_term_budgeted(&input, &baseline, || Ok(())).unwrap();
        assert!(expected
            .utf16()
            .ends_with(&"relat".encode_utf16().collect::<Vec<_>>()));
        assert_eq!(baseline.used(), expected.reserved_bytes());
        assert!(baseline.peak() > baseline.used());
        let mut partial_failures = 0;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match stem_term_budgeted(&input, &budget, || Ok(())) {
                Ok(actual) => assert_eq!(*actual, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                    if budget.peak() > 7 {
                        partial_failures += 1;
                    }
                }
                other => panic!("allowance {allowance}: {other:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
            assert_eq!(budget.used(), 0);
        }
        assert!(partial_failures > 3);
    }
}

#[test]
fn stemmer_cancellation_releases_each_loading_scanning_and_encoding_stage() {
    for input in [
        TokenTerm::from("y".repeat(8192)),
        TokenTerm::from("b".repeat(8192) + "ational"),
        TokenTerm::from("어".repeat(4096) + "relational"),
        TokenTerm::from_utf16(
            [
                vec![0xd800],
                vec![u16::from(b'y'); 8192],
                "ational".encode_utf16().collect(),
            ]
            .concat(),
        ),
    ] {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let expected = stem_term_budgeted(&input, &baseline, || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert!(polls > 20);
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let result = stem_term_budgeted(&input, &budget, || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "poll {stop}: {result:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
            assert_eq!(budget.used(), 0);
        }
        assert!(!expected.utf16().is_empty());
    }
}
