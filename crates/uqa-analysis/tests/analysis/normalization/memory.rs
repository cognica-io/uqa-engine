//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryBudget;

#[test]
fn every_normalization_callback_and_byte_failure_releases_scratch_and_retained_output() {
    let mut plans = vec![NormalizationConfig::CJKWidth];
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    plans.extend(profiles::plans());
    while let Some(plan) = plans.pop() {
        let compiled = keyword().with_normalization(plan).compile().unwrap();
        let input = "ＵＱＡ ｶﾞ İ ΟΣ 𐐀 ".repeat(90);
        let budget = MemoryBudget::new(usize::MAX);
        let mut calls = 0;
        let expected = compiled
            .normalize_budgeted(&input, &budget, || {
                calls += 1;
                Ok(())
            })
            .unwrap();
        let peak = budget.peak();
        assert_eq!(budget.used(), expected.reserved_bytes());
        assert_eq!(expected.reserved_bytes(), expected.capacity());
        assert!(calls > 3);
        for cutoff in 1..=calls {
            let budget = MemoryBudget::new(usize::MAX);
            let mut count = 0;
            let result = compiled.normalize_budgeted(&input, &budget, || {
                count += 1;
                if count == cutoff {
                    Err(AnalysisError::Descriptor("cancel normalization"))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(
                result,
                Err(AnalysisError::Descriptor("cancel normalization"))
            ));
            assert_eq!(budget.used(), 0, "callback {cutoff}");
        }
        let mut failures = 0;
        for allowance in (0..peak).step_by((peak / 19).max(1)).chain([peak]) {
            let budget = MemoryBudget::new(allowance + 7);
            let unrelated = budget.reserve(7).unwrap();
            match compiled.normalize_budgeted(&input, &budget, || Ok(())) {
                Ok(output) => {
                    assert_eq!(*output, *expected);
                    drop(output);
                }
                Err(AnalysisError::Memory(_)) => failures += 1,
                result => panic!("unexpected allowance outcome: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            drop(unrelated);
            assert_eq!(budget.used(), 0);
        }
        assert!(failures > 0);
        drop(expected);
        assert_eq!(budget.used(), 0);
    }
}
