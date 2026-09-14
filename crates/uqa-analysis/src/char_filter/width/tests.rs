//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete Lucene text/offset identities, source composition, and allocation ownership.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryBudget;

use crate::{AnalysisError, Analyzer, CharFilter, FilteredText, Tokenizer};

fn reference() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/cjk_width/expected.json"
    )))
    .unwrap()
}

fn integer(hash: &mut Sha256, value: usize) {
    hash.update(u32::try_from(value).unwrap().to_be_bytes());
}

fn text(hash: &mut Sha256, value: &str) {
    integer(hash, value.encode_utf16().count());
    for unit in value.encode_utf16() {
        hash.update(unit.to_be_bytes());
    }
}

fn hash_case(hash: &mut Sha256, input: &str) {
    let output = CharFilter::CJKWidth.filter_with_offsets(input).unwrap();
    text(hash, input);
    text(hash, output.as_str());
    let length = output.as_str().encode_utf16().count();
    integer(hash, length + 1);
    for offset in 0..=length {
        integer(
            hash,
            output
                .source_covering_offsets_utf16(offset..offset)
                .unwrap()
                .utf16
                .start,
        );
    }
}

#[test]
fn every_scalar_matches_lucene_text_and_every_corrected_utf16_boundary() {
    let input: String = (0..=0x10_ffff).filter_map(char::from_u32).collect();
    let reference = reference();
    assert_eq!(json!(input.chars().count()), reference["scalar_count"]);
    let mut hash = Sha256::new();
    hash_case(&mut hash, &input);
    assert_eq!(
        json!(format!("{:x}", hash.finalize())),
        reference["scalar_sha256"]
    );
}

#[test]
fn repeated_voiced_marks_match_immediate_emission_and_original_offsets() {
    let mut hash = Sha256::new();
    let mut count = 0;
    for range in [0x3000..=0x3100, 0xff00..=0xffa0] {
        for scalar in range {
            for first in ['\u{ff9e}', '\u{ff9f}'] {
                for second in ['\u{ff9e}', '\u{ff9f}'] {
                    let input: String = [char::from_u32(scalar).unwrap(), first, second]
                        .into_iter()
                        .collect();
                    hash_case(&mut hash, &input);
                    count += 1;
                }
            }
        }
    }
    let reference = reference();
    assert_eq!(json!(count), reference["combination_count"]);
    assert_eq!(
        json!(format!("{:x}", hash.finalize())),
        reference["combination_sha256"]
    );
}

#[test]
fn fixed_examples_preserve_exact_units_offsets_and_restricted_conversion() {
    let reference = reference();
    for case in reference["examples"].as_array().unwrap() {
        let units: Vec<u16> = serde_json::from_value(case["input"].clone()).unwrap();
        let input = String::from_utf16(&units).unwrap();
        let output = CharFilter::CJKWidth.filter_with_offsets(&input).unwrap();
        let actual: Vec<_> = output.as_str().encode_utf16().collect();
        assert_eq!(json!(actual), case["output"], "{input:?}");
        for (offset, expected) in case["offsets"].as_array().unwrap().iter().enumerate() {
            let source = output
                .source_covering_offsets_utf16(offset..offset)
                .unwrap();
            assert_eq!(json!(source.utf16.start), *expected, "{input:?}: {offset}");
            assert!(input.is_char_boundary(source.utf8.start));
            assert!(input.is_char_boundary(source.utf8.end));
        }
        assert_eq!(output.final_offsets().utf16.start, units.len());
    }
    assert_eq!(
        CharFilter::CJKWidth.filter("　①㍑ﬀ\u{212b}Ａ｡｢｣､").unwrap(),
        "　①㍑ﬀ\u{212b}A｡｢｣､"
    );
}

#[test]
fn expanded_source_edits_and_compiled_token_spans_survive_width_contraction() {
    let input = "🙂XＡ";
    let config = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 1,
            max_gram: 1,
        },
        Vec::new(),
        vec![
            CharFilter::Mapping {
                mapping: BTreeMap::from([("X".into(), "ｶﾞ".into())]),
            },
            CharFilter::CJKWidth,
        ],
    );
    let filtered = config.char_filters[0].filter_with_offsets(input).unwrap();
    let output = CharFilter::CJKWidth.filter_mapped(filtered).unwrap();
    assert_eq!(output.as_str(), "🙂ガA");
    assert_eq!(output.source_offsets(4..7).unwrap().utf8, 4..5);
    assert_eq!(output.source_offsets(4..7).unwrap().utf16, 2..3);
    assert_eq!(output.source_offsets(7..8).unwrap().utf8, 5..8);
    let uncompiled = config.analyze_tokens(input).unwrap();
    let compiled = config.compile().unwrap().analyze_tokens(input).unwrap();
    assert_eq!(compiled, uncompiled);
    assert_eq!(compiled.tokens().len(), 3);
}

#[test]
fn width_edits_keep_source_leases_and_release_only_their_own_failed_work() {
    let input = "🙂ｶﾞＡﾊﾟ";
    let baseline = MemoryBudget::new(1 << 20);
    let output = CharFilter::CJKWidth
        .filter_with_offsets_budgeted(input, &baseline, &mut || Ok(()))
        .unwrap();
    let retained = output.clone();
    drop(output);
    assert!(baseline.used() > 0);
    assert_eq!(retained.source_offsets(4..7).unwrap().utf8, 4..10);
    drop(retained);
    assert_eq!(baseline.used(), 0);

    for allowance in [0, 1, baseline.peak() / 2, baseline.peak() - 1] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        let result =
            CharFilter::CJKWidth.filter_with_offsets_budgeted(input, &budget, &mut || Ok(()));
        assert!(matches!(result, Err(AnalysisError::Memory(_))));
        assert_eq!(budget.used(), 7);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn cancellation_at_every_poll_preserves_the_borrowed_source_and_other_owners() {
    let input = "🙂ｶﾞＡﾊﾟ";
    let budget = MemoryBudget::new(1 << 20);
    let mut calls = 0;
    drop(
        CharFilter::CJKWidth
            .filter_with_offsets_budgeted(input, &budget, &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap(),
    );
    for stop in 1..=calls {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let source = FilteredText::new(input);
        let mut calls = 0;
        let result =
            CharFilter::CJKWidth.filter_mapped_budgeted(source.clone(), &budget, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(source.as_str(), input);
        assert_eq!(budget.used(), 7);
        let recovered = CharFilter::CJKWidth
            .filter_with_offsets_budgeted(input, &budget, &mut || Ok(()))
            .unwrap();
        assert_eq!(recovered.as_str(), "🙂ガAパ");
        drop(recovered);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn unchanged_long_input_polls_without_materializing_an_edit() {
    let input = "x🙂".repeat(4096);
    let mut text = FilteredText::new(&input);
    let budget = MemoryBudget::new(0);
    let mut calls = 0;
    super::replace_width(&mut text, &budget, &mut || {
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert!(calls >= 8192);
    assert_eq!(text.as_str(), input);
    assert_eq!(budget.used(), 0);
}
