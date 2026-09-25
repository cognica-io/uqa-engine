//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::read_control::StorageReadControl;

#[test]
fn controlled_cosine_keeps_independent_scores_and_sequential_rounding_across_chunks() {
    let control = StorageReadControl::with_limit(1);
    for (a, b, bits) in [
        (vec![1.0, 0.0], vec![3.0, 4.0], 0x3f19_999a),
        (vec![1.0, 0.0], vec![-1.0, 0.0], 0xbf80_0000),
        (vec![0.0, -0.0], vec![f32::MAX, 1.0], 0),
        (vec![f32::from_bits(1), 0.0], vec![1.0, 0.0], 0),
        (vec![1.0, 0.0], vec![f32::MAX, 0.0], 0),
    ] {
        assert_eq!(cosine_similarity(&a, &b).to_bits(), bits);
        assert_eq!(
            cosine_similarity_controlled(&a, &b, || control.check())
                .unwrap()
                .to_bits(),
            bits
        );
    }
    for count in [1024, 1025, 4097] {
        let mut a = vec![0.0; count];
        let mut b = vec![0.0; count];
        a[0] = 3.0;
        a[count - 1] = 4.0;
        b[0] = 1.0;
        // Zero padding across chunk boundaries leaves the exact norms 5 and 1, and dot 3.
        assert_eq!(cosine_similarity(&a, &b).to_bits(), 0x3f19_999a);
        assert_eq!(
            cosine_similarity_controlled(&a, &b, || control.check())
                .unwrap()
                .to_bits(),
            0x3f19_999a
        );
    }
    let mut a = vec![0.0; 2048];
    a[1023] = 16_777_216.0;
    a[1024] = 1.0;
    a[1025] = -16_777_216.0;
    let b = vec![1.0; a.len()];
    // Sequential f32 rounds 2^24 + 1 back to 2^24 before subtraction. Regrouping the two chunks would produce 1 instead of 0.
    assert_eq!(cosine_similarity(&a, &b).to_bits(), 0);
    assert_eq!(
        cosine_similarity_controlled(&a, &b, || control.check())
            .unwrap()
            .to_bits(),
        0
    );
    assert!(cosine_similarity(&[f32::MAX], &[f32::MAX]).is_nan());
    assert!(
        cosine_similarity_controlled(&[f32::MAX], &[f32::MAX], || control.check())
            .unwrap()
            .is_nan()
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_cosine_observes_cancellation_inside_a_long_vector_and_before_empty_work() {
    let values = vec![1.0; 4097];
    let control = StorageReadControl::with_limit(1);
    let mut checks = 0;
    assert!(cosine_similarity_controlled(&values, &values, || {
        checks += 1;
        if checks == 3 {
            control.cancellation().cancel();
        }
        control.check()
    })
    .is_err());
    assert_eq!(checks, 3);
    for (a, b) in [(&[][..], &[][..]), (&[1.0][..], &[][..])] {
        assert!(cosine_similarity_controlled(a, b, || control.check()).is_err());
        assert_eq!(cosine_similarity(a, b), 0.0);
    }
}
