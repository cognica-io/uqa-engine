//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryError;

fn reference(text: &str) -> String {
    CaseMapper::new()
        .lowercase_to_string(text, &icu_locale_core::LanguageIdentifier::UNKNOWN)
        .into_owned()
}

#[test]
fn contextual_sigma_ignores_case_ignorable_characters_on_both_sides() {
    let properties = prepare();
    let budget = MemoryBudget::new(4096);
    for (input, expected) in [
        ("Σ", "σ"),
        ("ΟΣ", "ος"),
        ("ΟΣΑ", "οσα"),
        ("ΟΣ1", "ος1"),
        ("AΣ\u{345}", "aς\u{345}"),
        ("AΣ\u{345}B", "aσ\u{345}b"),
        ("AΣ'", "aς'"),
        ("AΣ'B", "aσ'b"),
        ("A\u{345}Σ", "a\u{345}ς"),
        ("\u{345}Σ", "\u{345}σ"),
        ("AΣ\u{200d}B", "aσ\u{200d}b"),
        ("AΣΣ", "aσς"),
        ("ΣΣ", "σς"),
        ("ǅΣ", "ǆς"),
        ("İΣ", "i\u{307}ς"),
        ("AʕΣ", "aʕς"),
        ("AΣʕ", "aσʕ"),
        ("\u{1c89}", "\u{1c8a}"),
        ("\u{a7ce}", "\u{a7ce}"),
    ] {
        assert_eq!(reference(input), expected);
        let input = TokenTerm::from(input);
        let result = lower_budgeted(&input, properties, &budget, &mut || Ok(())).unwrap();
        assert_eq!(result.as_str(), Some(expected));
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn every_scalar_preserves_unicode_16_lowercase_and_whitespace() {
    let properties = prepare();
    let budget = MemoryBudget::new(1024);
    let mut cases = 0;
    let mut reference_hash = Sha256::new();
    for scalar in 0..=0x0010_ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        reference_hash.update(scalar.to_le_bytes());
        reference_hash.update([u8::from(crate::unicode::is_whitespace(character))]);
        for (prefix, suffix) in [("A", "Σ"), ("AΣ", ""), ("AΣ", "A")] {
            let input = format!("{prefix}{character}{suffix}");
            let expected = reference(&input);
            reference_hash.update((expected.len() as u64).to_le_bytes());
            reference_hash.update(expected.as_bytes());
            let result =
                lower_budgeted(&TokenTerm::from(input), properties, &budget, &mut || Ok(()))
                    .unwrap();
            assert_eq!(
                result.as_str(),
                Some(expected.as_str()),
                "U+{scalar:04X}, {prefix}/{suffix}"
            );
            assert_eq!(budget.used(), result.reserved_bytes());
            drop(result);
            assert_eq!(budget.used(), 0);
            cases += 1;
        }
    }
    assert_eq!(cases, 3_336_192);
    // Captured independently from Rust 1.90; see tests/fixtures/unicode-16-reference.rs.
    assert_eq!(
        format!("{:x}", reference_hash.finalize()),
        "b68614b465ba2c74ff335200b4ce7fad2c1105814ed284340e50c017f7b7eb15"
    );
}

#[test]
fn raw_units_separate_context_and_valid_pairs_remain_scalar_elements() {
    let properties = prepare();
    let budget = MemoryBudget::new(1024);
    for unit in 0..=u16::MAX {
        for (prefix, suffix) in [("A", "Σ"), ("AΣ", "A")] {
            let units = [
                vec![0xd800],
                prefix.encode_utf16().collect(),
                vec![unit],
                suffix.encode_utf16().collect(),
                vec![0xdc00],
            ]
            .concat();
            let input = TokenTerm::from_utf16(units);
            let expected = input.map_unicode(reference);
            let result = lower_budgeted(&input, properties, &budget, &mut || Ok(())).unwrap();
            assert_eq!(*result, expected, "unit {unit:04X}");
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
    for units in [
        vec![0xd800, 65, 0x03a3, 0xd801, 0xdc00],
        vec![0xd800, 0xd801, 0xdc00, 0x03a3],
    ] {
        let input = TokenTerm::from_utf16(units);
        let expected = input.map_unicode(reference);
        let result = lower_budgeted(&input, properties, &budget, &mut || Ok(())).unwrap();
        assert_eq!(*result, expected);
    }
}

#[test]
fn case_expansion_limits_release_every_output_buffer() {
    let properties = prepare();
    for input in [
        TokenTerm::from("İAΣ\u{345}B🙂ΟΣ"),
        TokenTerm::from_utf16([vec![0xd800], "İAΣ\u{345}B🙂ΟΣ".encode_utf16().collect()].concat()),
    ] {
        let baseline = MemoryBudget::new(4096);
        let expected = lower_budgeted(&input, properties, &baseline, &mut || Ok(())).unwrap();
        assert_eq!(*expected, input.map_unicode(reference));
        let mut failures = 0;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match lower_budgeted(&input, properties, &budget, &mut || Ok(())) {
                Ok(actual) => assert_eq!(*actual, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                    failures += 1;
                }
                other => panic!("allowance {allowance}: {other:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(failures > 3);
    }
}

#[test]
fn long_ignorable_context_uses_bounded_forward_scans_and_interruptible_output() {
    let properties = prepare();
    let source = "AΣ".to_owned() + &"\u{345}".repeat(8192) + "AΣ" + &"\u{301}".repeat(8192);
    for input in [
        TokenTerm::from(source.clone()),
        TokenTerm::from_utf16(
            [vec![0xd800], source.encode_utf16().collect(), vec![0xdc00]].concat(),
        ),
    ] {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let expected = lower_budgeted(&input, properties, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(*expected, input.map_unicode(reference));
        assert!(polls > 20 && polls < 100);
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let actual = lower_budgeted(&input, properties, &budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(actual, Err(AnalysisError::Cancelled)),
                "poll {stop}: {actual:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[path = "tests/unicode_17.rs"]
mod unicode_17;
