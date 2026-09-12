//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::AnalysisError;

fn bytes(term: &TokenTerm) -> usize {
    match &term.0 {
        Representation::Unicode(text) => text.capacity(),
        Representation::UTF16(units) => units.capacity() * size_of::<u16>(),
    }
}

#[test]
fn internal_order_preserves_canonical_identity_and_long_prefix_order() {
    let prefix = "한🙂".repeat(2049);
    let values = [
        TokenTerm::from(""),
        TokenTerm::from("a"),
        TokenTerm::from_utf16("a".encode_utf16().collect()),
        TokenTerm::from(prefix.clone()),
        TokenTerm::from(format!("{prefix}a")),
        TokenTerm::from(format!("{prefix}b")),
        TokenTerm::from_utf16(vec![0xd800]),
        TokenTerm::from_utf16([vec![0xd800], prefix.encode_utf16().collect()].concat()),
        TokenTerm::from_utf16([vec![0xd800], prefix.encode_utf16().collect(), vec![97]].concat()),
        TokenTerm::from_utf16(vec![0xd801]),
    ];
    for (left_index, left) in values.iter().enumerate() {
        for (right_index, right) in values.iter().enumerate() {
            let order = left.cmp_with_control(right, &mut || Ok(())).unwrap();
            assert_eq!(order.is_eq(), left == right);
            if left != right {
                assert_eq!(order, left_index.cmp(&right_index));
            }
            assert_eq!(
                order.reverse(),
                right.cmp_with_control(left, &mut || Ok(())).unwrap()
            );
        }
    }
    for input in [&values[5], &values[8]] {
        let mut calls = 0;
        assert!(input
            .cmp_with_control(input, &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap()
            .is_eq());
        assert!(calls > 5);
        for stop in 1..=calls {
            let mut count = 0;
            assert!(matches!(
                input.cmp_with_control(input, &mut || {
                    count += 1;
                    if count == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(AnalysisError::Cancelled)
            ));
        }
    }
}

#[test]
fn complete_boundary_layout_fits_without_transient_replacement_buffers() {
    let text = "韓🙂a".repeat(2000);
    for input in [
        TokenTerm::from(text.clone()),
        TokenTerm::from_utf16([vec![0xd800], text.encode_utf16().collect()].concat()),
    ] {
        let count = input.character_count() + 1;
        let required = count * size_of::<TermBoundary>();
        let budget = MemoryBudget::new(required + 7);
        let other = budget.reserve(7).unwrap();
        let boundaries = input.boundaries_budgeted(&budget, &mut || Ok(())).unwrap();
        assert_eq!(boundaries.len(), count);
        assert_eq!(boundaries.reserved_bytes(), required);
        assert_eq!(budget.peak(), required + 7);
        drop(boundaries);
        assert_eq!(budget.used(), 7);
        drop(other);
    }
}

#[test]
fn small_utf16_terms_keep_canonical_identity_and_both_boundary_coordinates() {
    let alphabet = [0x0061, 0x97d3, 0xd800, 0xdc00, 0xd83d, 0xde42];
    let budget = MemoryBudget::new(4096);
    let mut cases = 0;
    for length in 0..=5 {
        for mut index in 0..alphabet.len().pow(length) {
            let units: Vec<_> = (0..length)
                .map(|_| {
                    let unit = alphabet[index % alphabet.len()];
                    index /= alphabet.len();
                    unit
                })
                .collect();
            let input = TokenTerm::from_utf16(units);
            let cloned = input.clone_budgeted(&budget, || Ok(())).unwrap();
            assert_eq!(*cloned, input);
            assert_eq!(cloned.reserved_bytes(), bytes(&cloned));
            drop(cloned);
            let boundaries = input.boundaries_budgeted(&budget, &mut || Ok(())).unwrap();
            assert_eq!(
                boundaries
                    .iter()
                    .map(|value| value.offset)
                    .collect::<Vec<_>>(),
                input.boundaries()
            );
            assert_eq!(
                boundaries.reserved_bytes(),
                boundaries.capacity() * size_of::<TermBoundary>()
            );
            for (start, left) in boundaries.iter().enumerate() {
                assert_eq!(left.utf16, input.substring(0..left.offset).utf16_len());
                for right in &boundaries[start..] {
                    let range = left.offset..right.offset;
                    let output = input
                        .substring_budgeted(range.clone(), &budget, &mut || Ok(()))
                        .unwrap();
                    assert_eq!(*output, input.substring(range));
                    assert_eq!(output.utf16_len(), right.utf16 - left.utf16);
                    assert_eq!(output.reserved_bytes(), bytes(&output));
                    assert_eq!(
                        budget.used(),
                        boundaries.reserved_bytes() + output.reserved_bytes()
                    );
                }
            }
            drop(boundaries);
            assert_eq!(budget.used(), 0);
            cases += 1;
        }
    }
    assert_eq!(cases, 9331);
}

#[test]
fn term_copy_and_boundary_failures_leave_the_source_and_other_reservations_intact() {
    for input in [
        TokenTerm::from("a韓🙂z"),
        TokenTerm::from_utf16(vec![0xd800, 97, 0xd83d, 0xde42, 0xdc00]),
    ] {
        let original = input.clone();
        let run = |budget: &MemoryBudget, poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            let copied = input.clone_budgeted(budget, &mut *poll)?;
            let boundaries = copied.boundaries_budgeted(budget, poll)?;
            copied.substring_budgeted(
                boundaries[1].offset..boundaries[boundaries.len() - 2].offset,
                budget,
                poll,
            )
        };
        let baseline = MemoryBudget::new(4096);
        let expected = run(&baseline, &mut || Ok(())).unwrap();
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match run(&budget, &mut || Ok(())) {
                Ok(value) => assert_eq!(*value, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                value => panic!("allowance {allowance}: {value:?}"),
            }
            assert_eq!(input, original);
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
    }
}

#[test]
fn long_term_copy_comparison_boundaries_and_slicing_are_interruptible() {
    let text = "A韓🙂".repeat(2048);
    for input in [
        TokenTerm::from(text.clone()),
        TokenTerm::from_utf16([vec![0xd800], text.encode_utf16().collect(), vec![0xdc00]].concat()),
    ] {
        let run = |budget: &MemoryBudget, poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            let copied = input.clone_budgeted(budget, &mut *poll)?;
            assert!(input.eq_with_control(&copied, poll)?);
            let boundaries = copied.boundaries_budgeted(budget, poll)?;
            let start = usize::from(input.as_str().is_none());
            let end = boundaries.len() - 1 - start;
            copied.substring_budgeted(
                boundaries[start].offset..boundaries[end].offset,
                budget,
                poll,
            )
        };
        let mut calls = 0;
        let baseline = MemoryBudget::new(1 << 20);
        let output = run(&baseline, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(output.as_str(), Some(text.as_str()));
        assert!(calls > 20);
        drop(output);
        assert_eq!(baseline.used(), 0);
        for stop in 1..=calls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let result = run(&budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "callback {stop}: {result:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
        let other = TokenTerm::from("other");
        assert!(!input.eq_with_control(&other, &mut || Ok(())).unwrap());
    }
}
