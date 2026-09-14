//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::{AnalysisError, TokenTerm};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError};

fn input(units: &[u16], budget: &MemoryBudget) -> Budgeted<Vec<u16>> {
    let mut buffer = BudgetedVec::new(budget);
    buffer.reserve(units.len() + 3).unwrap();
    for unit in units {
        buffer.push(*unit).unwrap();
    }
    let (buffer, memory) = buffer.into_parts();
    Budgeted::new(buffer, memory)
}

#[test]
fn canonical_terms_release_replaced_capacity_and_preserve_unpaired_units() {
    for units in [
        vec![],
        vec![65],
        vec![0xd55c, 0xd83d, 0xde42],
        vec![65, 0xd83d],
        vec![0xde42, 65],
    ] {
        let budget = MemoryBudget::new(1 << 20);
        let owned = input(&units, &budget);
        let input_bytes = owned.reserved_bytes();
        let expected = TokenTerm::from_utf16(units.clone());
        let output = TokenTerm::from_utf16_budgeted(owned, || Ok(())).unwrap();
        assert_eq!(*output, expected);
        assert_eq!(output.utf16().as_ref(), units);
        if let Some(text) = output.as_str() {
            assert_eq!(budget.used(), text.len());
            assert_eq!(budget.peak(), input_bytes + text.len());
        } else {
            assert_eq!(budget.used(), input_bytes);
            assert_eq!(budget.peak(), input_bytes);
        }
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn term_conversion_requires_both_encodings_to_fit_and_preserves_other_owners() {
    let budget = MemoryBudget::new(7 + 8 + 2);
    let other = budget.reserve(7).unwrap();
    let owned = input(&[0xd55c], &budget);
    assert!(matches!(
        TokenTerm::from_utf16_budgeted(owned, || Ok(())),
        Err(AnalysisError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(budget.used(), 7);
    assert_eq!(budget.peak(), 15);
    drop(other);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_releases_units_and_partially_decoded_text() {
    let units: Vec<_> = "한🙂".repeat(2048).encode_utf16().collect();
    let baseline = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let expected = TokenTerm::from_utf16_budgeted(input(&units, &baseline), || {
        polls += 1;
        Ok(())
    })
    .unwrap();
    assert!(polls > 8);
    for stop in 1..=polls {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        let actual = TokenTerm::from_utf16_budgeted(input(&units, &budget), || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(actual, Err(AnalysisError::Cancelled)),
            "poll {stop}"
        );
        assert_eq!(budget.used(), 7);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(expected.as_str(), Some("한🙂".repeat(2048).as_str()));
}
