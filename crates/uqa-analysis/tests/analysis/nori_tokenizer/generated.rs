//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated valid Unicode inputs exercise stream invariants and interrupted-call reuse.

use super::{model, KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits};
use uqa_analysis::nori::{DecompoundMode, NoriLimits};
use uqa_analysis::AnalysisError;
use uqa_core::memory::MemoryBudget;

fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 32) as u32
}

fn input(seed: usize) -> String {
    let pieces = [
        "한국",
        "세종시",
        "韓國",
        "감싸여",
        "🙂a",
        "\u{1112}\u{1161}\u{11ab}",
        "e\u{301}",
        "A9",
        " ",
        "\t\n",
        "\0",
        "\u{d7ff}",
        "\u{e000}",
        "\u{ffff}",
        "\u{10000}",
        "\u{10ffff}",
        "\u{200d}",
        "!.",
        "𝔘",
        "𠀀",
    ];
    let mut state = seed as u64 + 0x6a09_e667_f3bc_c909;
    let mut text = String::new();
    for _ in 0..[0, 1, 2, 3, 31, 63, 127, 257][seed % 8] {
        if next(&mut state).is_multiple_of(4) {
            if let Some(scalar) = char::from_u32(next(&mut state) % 0x11_0000) {
                text.push(scalar);
            }
        } else {
            text.push_str(pieces[next(&mut state) as usize % pieces.len()]);
        }
    }
    if seed % 8 == 7 {
        text.push_str(&"가".repeat(1030));
    }
    text
}

#[test]
fn generated_unicode_streams_preserve_coordinates_allowances_and_reuse() {
    let user = UserDictionary::compile(
        "🙂a 가 나\n세종시 세종 시\n",
        model(),
        UserDictionaryLimits::default(),
    )
    .unwrap();
    for seed in 0..64 {
        let input = input(seed);
        let units = input.encode_utf16().count();
        for mode in [
            DecompoundMode::None,
            DecompoundMode::Discard,
            DecompoundMode::Mixed,
        ] {
            for output_unknown_unigrams in [false, true] {
                for discard_punctuation in [false, true] {
                    let options = NoriOptions {
                        decompound_mode: mode,
                        output_unknown_unigrams,
                        discard_punctuation,
                    };
                    let tokenizer =
                        KoreanTokenizer::new(model().clone(), user.clone(), options).unwrap();
                    let budget = MemoryBudget::new(usize::MAX);
                    let other = budget.reserve(13).unwrap();
                    let mut polls = 0;
                    let result = tokenizer
                        .tokenize_budgeted(&input, NoriLimits::default(), &budget, &mut || {
                            polls += 1;
                            Ok(())
                        })
                        .unwrap_or_else(|error| panic!("seed {seed}, {options:?}: {error}"));
                    assert_eq!(result.final_offset_utf16, units, "seed {seed}, {options:?}");
                    for token in &result.tokens {
                        assert!(
                            token.start_utf16 <= token.end_utf16 && token.end_utf16 <= units,
                            "seed {seed}, {options:?}: {token:?}"
                        );
                        assert!(
                            token.position_length > 0,
                            "seed {seed}, {options:?}: {token:?}"
                        );
                    }
                    super::super::nori_resources::assert_generic_bridge(&result, &input);
                    assert_eq!(budget.used(), 13 + result.reserved_bytes());
                    let expected = (*result).clone();
                    drop(result);
                    assert_eq!(budget.used(), 13);
                    if !input.is_empty() {
                        assert!(polls > 0);
                    }
                    if polls > 0 {
                        for stop in [1, (polls / 2).max(1), polls] {
                            let mut calls = 0;
                            let failed = tokenizer.tokenize_budgeted(
                                &input,
                                NoriLimits::default(),
                                &budget,
                                &mut || {
                                    calls += 1;
                                    if calls == stop {
                                        Err(AnalysisError::Cancelled)
                                    } else {
                                        Ok(())
                                    }
                                },
                            );
                            assert!(
                                matches!(failed, Err(AnalysisError::Cancelled)),
                                "seed {seed}, {options:?}, poll {stop}"
                            );
                            assert_eq!(calls, stop);
                            assert_eq!(budget.used(), 13);
                        }
                    }
                    assert_eq!(
                        tokenizer.tokenize(&input).unwrap(),
                        expected,
                        "seed {seed}, {options:?}"
                    );
                    drop(other);
                    assert_eq!(budget.used(), 0);
                }
            }
        }
    }
}
