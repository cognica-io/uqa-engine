//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{AnalysisError, AnalysisResult};

#[test]
fn reserved_key_encoding_and_copies_keep_canonical_bytes_and_unique_buffers() {
    for term in [
        TokenTerm::from(""),
        TokenTerm::from("韓🙂\0"),
        TokenTerm::from_utf16(vec![0xd800, 97, 0xdc00]),
        TokenTerm::from_utf16(vec![0xd83d, 0xde42]),
    ] {
        let expected = TokenTermKey::from_term(&term);
        let size = expected.as_bytes().len();
        for allowance in 0..=size * 2 {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            let run = || -> AnalysisResult<()> {
                let key = TokenTermKey::from_term_budgeted(&term, &budget, || {
                    Ok::<(), AnalysisError>(())
                })?;
                assert_eq!(*key, expected);
                assert_eq!(key.reserved_bytes(), key.0.capacity());
                let copy = key.clone_budgeted(&budget, || Ok::<(), AnalysisError>(()))?;
                assert_eq!(*copy, expected);
                assert_ne!(copy.0.as_ptr(), key.0.as_ptr());
                assert_eq!(budget.used(), key.0.capacity() + copy.0.capacity() + 7);
                drop(key);
                assert_eq!(budget.used(), copy.reserved_bytes() + 7);
                Ok(())
            };
            match run() {
                Ok(()) => assert!(allowance >= size * 2),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                    assert!(allowance < size * 2);
                }
                result => panic!("{result:?}"),
            }
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn long_key_encoding_copying_and_persistent_order_are_interruptible() {
    let prefix = "韓🙂".repeat(2049);
    let terms = [
        TokenTerm::from(prefix.clone()),
        TokenTerm::from(format!("{prefix}a")),
        TokenTerm::from_utf16([vec![0xd800], prefix.encode_utf16().collect()].concat()),
        TokenTerm::from_utf16([vec![0xd800], prefix.encode_utf16().collect(), vec![97]].concat()),
    ];
    for term in &terms {
        let expected = TokenTermKey::from_term(term);
        let run = |budget: &MemoryBudget, poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            let key = TokenTermKey::from_term_budgeted(term, budget, &mut *poll)?;
            let copy = key.clone_budgeted(budget, &mut *poll)?;
            assert_eq!(*copy, expected);
            assert!(key.cmp_with_control(&copy, poll)?.is_eq());
            Ok::<_, AnalysisError>(copy)
        };
        let budget = MemoryBudget::new(1 << 20);
        let mut calls = 0;
        drop(
            run(&budget, &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap(),
        );
        assert!(calls > 10);
        for stop in 1..=calls {
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            assert!(matches!(
                run(&budget, &mut || {
                    count += 1;
                    if count == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(AnalysisError::Cancelled)
            ));
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
    let keys: Vec<_> = terms.iter().map(TokenTermKey::from_term).collect();
    for left in &keys {
        for right in &keys {
            assert_eq!(
                left.cmp_with_control(right, &mut || Ok::<(), ()>(()))
                    .unwrap(),
                left.cmp(right)
            );
        }
    }
}
