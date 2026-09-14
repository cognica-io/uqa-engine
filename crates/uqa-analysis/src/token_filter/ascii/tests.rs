//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::AnalysisError;
use unicode_normalization::UnicodeNormalization;
use uqa_core::memory::MemoryError;

fn reference_fold(input: &str) -> String {
    let mut output = String::new();
    for character in input.chars() {
        let folded: String = character.nfkd().filter(char::is_ascii).collect();
        if folded.is_empty() {
            output.push(character);
        } else {
            output.push_str(&folded);
        }
    }
    output
}

#[test]
fn every_scalar_matches_per_character_nfkd_and_original_fallback() {
    let budget = MemoryBudget::new(1024);
    for scalar in 0..=0x0010_ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        let input = character.to_string();
        let expected = reference_fold(&input);
        let term = TokenTerm::from(input);
        let actual = fold_budgeted(&term, &budget, &mut || Ok(())).unwrap();
        assert_eq!(actual.as_str(), Some(expected.as_str()), "U+{scalar:04X}");
        assert_eq!(budget.used(), actual.reserved_bytes());
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn lossless_folding_reserves_expansions_and_preserves_other_owners_on_failure() {
    let source = "ﬃ㍍é韓🙂 \u{fdfa}";
    for input in [
        TokenTerm::from(source),
        TokenTerm::from_utf16(
            [vec![0xd800], source.encode_utf16().collect(), vec![0xdc00]].concat(),
        ),
    ] {
        let reference = input.map_unicode(reference_fold);
        let baseline = MemoryBudget::new(4096);
        let expected = fold_budgeted(&input, &baseline, &mut || Ok(())).unwrap();
        assert_eq!(*expected, reference);
        assert_eq!(baseline.used(), expected.reserved_bytes());
        let mut failures = 0;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match fold_budgeted(&input, &budget, &mut || Ok(())) {
                Ok(actual) => assert_eq!(*actual, reference),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                    failures += 1;
                }
                other => panic!("allowance {allowance}: {other:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
            assert_eq!(budget.used(), 0);
        }
        assert!(failures > 3);
    }
}

#[test]
fn cancellation_drops_partial_scalar_and_raw_term_buffers() {
    let source = "ﬃé韓🙂 \u{fdfa}".repeat(1024);
    for input in [
        TokenTerm::from(source.clone()),
        TokenTerm::from_utf16([vec![0xd800], source.encode_utf16().collect()].concat()),
    ] {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let expected = fold_budgeted(&input, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(*expected, input.map_unicode(reference_fold));
        assert!(polls > 4);
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let actual = fold_budgeted(&input, &budget, &mut || {
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
