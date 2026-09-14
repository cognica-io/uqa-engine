//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{input, units};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uqa_analysis::nori::{
    normalize_number_budgeted, normalize_number_utf16_budgeted, DictionaryError, NoriLimits,
};
use uqa_analysis::AnalysisError;
use uqa_core::memory::{MemoryBudget, MemoryError};

#[test]
fn reserved_numeric_normalization_matches_every_pinned_normalization_snapshot() {
    let cases: Vec<Value> = serde_json::from_slice(include_bytes!(
        "../../../../../tests/parity/nori/number_cases.json"
    ))
    .unwrap();
    let expected = include_str!("../../../../../tests/parity/nori/number_expected.jsonl");
    let mut checked = 0;
    for (case, expected) in cases.iter().zip(expected.lines()) {
        if case["pipeline"] != "normalize" {
            continue;
        }
        let expected: Value = serde_json::from_str(expected).unwrap();
        assert_eq!(case["id"], expected["id"]);
        let source = input(case);
        let original = source.clone();
        let budget = MemoryBudget::new(64 << 20);
        let other = budget.reserve(7).unwrap();
        let output = normalize_number_utf16_budgeted(
            &source,
            NoriLimits::default(),
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(source, original);
        assert_eq!(
            output.len(),
            expected["unit_count"].as_u64().unwrap() as usize
        );
        if let Some(expected) = expected.get("normalized_utf16") {
            assert_eq!(*output, units(expected), "{}", case["id"]);
        }
        let mut hash = Sha256::new();
        for unit in output.iter() {
            hash.update(unit.to_be_bytes());
        }
        assert_eq!(
            format!("{:x}", hash.finalize()),
            expected["utf16be_sha256"].as_str().unwrap()
        );
        assert_eq!(
            output.reserved_bytes(),
            output.capacity() * size_of::<u16>()
        );
        assert_eq!(budget.used(), output.reserved_bytes() + 7);
        drop(output);
        assert_eq!(budget.used(), 7);
        drop(other);
        checked += 1;
    }
    assert_eq!(checked, 334, "normalization fixture coverage");
}

#[test]
fn numeral_limits_never_release_other_owners_or_turn_allocation_errors_into_fallback() {
    for source in [
        "",
        "0",
        "000",
        "1.2.3",
        "1.23백45.600천",
        "9해9경9조",
        "0.00001",
        "garbage",
        "１２원",
    ] {
        let units: Vec<_> = source.encode_utf16().collect();
        let baseline = MemoryBudget::new(1 << 20);
        let expected =
            normalize_number_utf16_budgeted(&units, NoriLimits::default(), &baseline, &mut || {
                Ok(())
            })
            .unwrap();
        let peak = baseline.peak();
        let mut succeeded = false;
        for allowance in 0..=peak {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match normalize_number_utf16_budgeted(
                &units,
                NoriLimits::default(),
                &budget,
                &mut || Ok(()),
            ) {
                Ok(output) => {
                    assert_eq!(*output, *expected, "{source:?}, allowance {allowance}");
                    assert_eq!(budget.used(), output.reserved_bytes() + 7);
                    succeeded = true;
                }
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                result => panic!("{source:?}, allowance {allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(succeeded);
    }
    let budget = MemoryBudget::new(1 << 20);
    for (source, limits) in [
        (
            "1.2.3",
            NoriLimits {
                max_output_utf16: 2,
                ..NoriLimits::default()
            },
        ),
        (
            "해",
            NoriLimits {
                max_output_utf16: 1,
                ..NoriLimits::default()
            },
        ),
        (
            "100",
            NoriLimits {
                max_input_utf16: 1,
                ..NoriLimits::default()
            },
        ),
    ] {
        assert!(matches!(
            normalize_number_budgeted(source, limits, &budget, &mut || Ok(())),
            Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
        ));
        assert_eq!(budget.used(), 0);
    }
    let raw = [0xd800, 0xff11, 0xdc00];
    let output =
        normalize_number_utf16_budgeted(&raw, NoriLimits::default(), &budget, &mut || Ok(()))
            .unwrap();
    assert_eq!(*output, raw);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn long_numeric_coefficients_and_both_encodings_are_cancellable_with_exact_reservations() {
    let source = format!("{}{}", "9".repeat(4096), "십".repeat(4096));
    for scalar in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let units: Vec<_> = source.encode_utf16().collect();
        let mut calls = 0;
        if scalar {
            let result =
                normalize_number_budgeted(&source, NoriLimits::default(), &budget, &mut || {
                    calls += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(budget.used(), result.reserved_bytes());
            assert_eq!(result.reserved_bytes(), result.capacity());
        } else {
            let result = normalize_number_utf16_budgeted(
                &units,
                NoriLimits::default(),
                &budget,
                &mut || {
                    calls += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(budget.used(), result.reserved_bytes());
            assert_eq!(
                result.reserved_bytes(),
                result.capacity() * size_of::<u16>()
            );
        }
        assert_eq!(budget.used(), 0);
        assert!(
            calls > 10 && calls < 300,
            "repeated coefficient scans: {calls}"
        );
        for stop in 1..=calls {
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let mut poll = || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            };
            let result = if scalar {
                normalize_number_budgeted(&source, NoriLimits::default(), &budget, &mut poll)
                    .map(drop)
            } else {
                normalize_number_utf16_budgeted(&units, NoriLimits::default(), &budget, &mut poll)
                    .map(drop)
            };
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "scalar {scalar}, callback {stop}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn a_known_coefficient_reserves_once_and_fits_exact_scratch_plus_output() {
    let input = vec![u16::from(b'9'); 8192];
    let budget = MemoryBudget::new(input.len() * 3 + 7);
    let other = budget.reserve(7).unwrap();
    let output =
        normalize_number_utf16_budgeted(&input, NoriLimits::default(), &budget, &mut || Ok(()))
            .unwrap();
    assert_eq!(*output, input);
    assert_eq!(budget.used(), input.len() * 2 + 7);
    assert_eq!(budget.peak(), budget.limit());
    drop(output);
    assert_eq!(budget.used(), 7);
    drop(other);
}
