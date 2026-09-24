//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};
use num_bigint::Sign;

fn native_limbs(limbs: usize, zero_prefix: usize) -> BigInt {
    let words_per_limb = size_of::<usize>() / size_of::<u32>();
    let mut words = vec![0xffff_ffff; limbs * words_per_limb];
    words[..zero_prefix * words_per_limb].fill(0);
    BigInt::new(Sign::Plus, words)
}

#[test]
fn admitted_kernels_preserve_native_results_across_multiplication_and_normalization_branches() {
    let budget = MemoryBudget::new(64 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (left_limbs, right_limbs, right_zeros) in [
        (1, 1, 0),
        (32, 33, 0),
        (33, 65, 0),
        (33, 66, 0),
        (256, 257, 0),
        (257, 257, 0),
        (257, 1024, 0),
        (512, 1024, 424),
    ] {
        let left = native_limbs(left_limbs, 0);
        let right = native_limbs(right_limbs, right_zeros);
        let output = multiply(&left, &right, &control).unwrap();
        assert_eq!(&*output, &(&left * &right));
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
        let sum = add(&left, &(-&right), &control).unwrap();
        assert_eq!(&*sum, &(&left - &right));
        drop(sum);
        let quotient = divide(&right, &left, &control).unwrap();
        assert_eq!(&*quotient, &(&right / &left));
        drop(quotient);
        let rest = remainder(&right, &left, &control).unwrap();
        assert_eq!(&*rest, &(&right % &left));
        drop(rest);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn kernel_admission_precedes_execution_and_both_tokens_preserve_resource_errors() {
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let invoked = std::cell::Cell::new(false);
    let output = run(
        &control,
        || Ok(1),
        || {
            invoked.set(true);
            BigInt::from(1)
        },
    );
    assert!(matches!(output, Err(ValueRetentionError::Memory(_))));
    assert!(!invoked.get());
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let output = multiply(&BigInt::from(2), &BigInt::from(3), &control);
        assert!(matches!(output, Err(ValueRetentionError::Cancelled(_))));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn cancellation_after_native_kernel_releases_its_value_before_returning() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let output = run(
        &control,
        || Ok(64),
        || {
            let value = BigInt::from(12345);
            invoking.cancel();
            value
        },
    );
    assert!(matches!(output, Err(ValueRetentionError::Cancelled(_))));
    assert_eq!(budget.used(), 0);
}
