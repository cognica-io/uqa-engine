//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::unicode::Profile;

#[test]
fn every_scalar_preserves_historical_unicode_17_lowercase_and_whitespace() {
    let properties = prepare_with_profile(Profile::Unicode17);
    let budget = MemoryBudget::new(1024);
    let mut hash = Sha256::new();
    let mut cases = 0;
    for scalar in 0..=0x0010_ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        hash.update(scalar.to_le_bytes());
        hash.update([u8::from(crate::unicode::is_whitespace(character))]);
        for (prefix, suffix) in [("A", "Σ"), ("AΣ", ""), ("AΣ", "A")] {
            let input = TokenTerm::from(format!("{prefix}{character}{suffix}"));
            let actual = lower_budgeted(&input, properties, &budget, &mut || Ok(())).unwrap();
            let text = actual.as_str().unwrap();
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
            assert_eq!(budget.used(), actual.reserved_bytes());
            drop(actual);
            assert_eq!(budget.used(), 0);
            cases += 1;
        }
    }
    assert_eq!(cases, 3_336_192);
    // Captured from the released pre-pinning implementation; see tests/fixtures/unicode-17-reference.md.
    assert_eq!(
        format!("{:x}", hash.finalize()),
        "549ed6a569bd3f7c57a55eb65a02272fde54b852b0f9b673275225f1dc90733c"
    );
}

#[test]
fn retained_unicode_17_preserves_raw_units_and_releases_failed_reservations() {
    let properties = prepare_with_profile(Profile::Unicode17);
    let source = "İ\u{a7ce}Σ AΣ\u{a7ce} \u{16ea0}";
    let expected = "i\u{307}\u{a7cf}σ aς\u{a7cf} \u{16ebb}";
    for (input, expected) in [
        (TokenTerm::from(source), TokenTerm::from(expected)),
        (
            TokenTerm::from_utf16(
                [vec![0xd800], source.encode_utf16().collect(), vec![0xdc00]].concat(),
            ),
            TokenTerm::from_utf16(
                [
                    vec![0xd800],
                    expected.encode_utf16().collect(),
                    vec![0xdc00],
                ]
                .concat(),
            ),
        ),
    ] {
        let baseline = MemoryBudget::new(4096);
        assert_eq!(
            *lower_budgeted(&input, properties, &baseline, &mut || Ok(())).unwrap(),
            expected
        );
        let mut failures = 0;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match lower_budgeted(&input, properties, &budget, &mut || Ok(())) {
                Ok(actual) => assert_eq!(*actual, expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => failures += 1,
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
fn retained_unicode_17_keeps_linear_context_work_and_cancellation() {
    let properties = prepare_with_profile(Profile::Unicode17);
    let source = "\u{a7ce}Σ".to_owned() + &"\u{345}".repeat(8192) + "AΣ\u{16ea0}";
    let expected = "\u{a7cf}σ".to_owned() + &"\u{345}".repeat(8192) + "aς\u{16ebb}";
    let input = TokenTerm::from(source);
    let budget = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let output = lower_budgeted(&input, properties, &budget, &mut || {
        polls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(output.as_str(), Some(expected.as_str()));
    assert!(polls > 8 && polls < 100, "{polls}");
    drop(output);
    for stop in 1..=polls {
        let mut count = 0;
        let output = lower_budgeted(&input, properties, &budget, &mut || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(output, Err(AnalysisError::Cancelled)));
        assert_eq!(budget.used(), 0);
    }
}
