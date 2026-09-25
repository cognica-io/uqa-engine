//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use proptest::prelude::*;

use super::*;
use crate::vector_index::{cosine_similarity, DiskANNAlpha};

fn navigable(raw: &[f32], control: &StorageReadControl) -> NavigationVector {
    match NavigationInput::from_raw(raw.len() as u32, raw, control).unwrap() {
        NavigationInput::Navigable(vector) => vector,
        NavigationInput::Exact(reason) => panic!("unexpected exact route: {reason:?}"),
    }
}

#[test]
fn normalization_preserves_raw_bits_and_canonical_scores() {
    let control = StorageReadControl::with_limit(1024);
    let raw = [-0.0_f32, 3.0, 4.0];
    let bits = raw.map(f32::to_bits);
    let query = [0.0, 1.0, 0.0];
    let vector = navigable(&raw, &control);
    assert_eq!(raw.map(f32::to_bits), bits);
    assert_eq!(vector.coordinates()[0].to_bits(), (-0.0_f64).to_bits());
    assert_eq!(vector.coordinates(), &[-0.0, 0.6, 0.8]);
    assert_eq!(cosine_similarity(&query, &raw).to_bits(), 0x3f19_999a);
    assert_eq!(cosine_similarity(&[-1.0], &[1.0]).to_bits(), 0xbf80_0000);
    assert_eq!(cosine_similarity(&[-0.0], &[1.0]).to_bits(), 0);
    drop(vector);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn exceptional_raw_norms_remain_explicit_exact_inputs() {
    let control = StorageReadControl::with_limit(0);
    for raw in [[0.0, -0.0], [1.0e-30, 0.0], [f32::from_bits(1), 0.0]] {
        assert!(matches!(
            NavigationInput::from_raw(2, &raw, &control).unwrap(),
            NavigationInput::Exact(ExactVectorReason::ZeroNorm)
        ));
        assert_eq!(cosine_similarity(&raw, &[1.0, 0.0]).to_bits(), 0);
    }
    for raw in [[f32::MAX, 0.0], [1.0e20, 1.0e20]] {
        assert!(matches!(
            NavigationInput::from_raw(2, &raw, &control).unwrap(),
            NavigationInput::Exact(ExactVectorReason::NonFiniteNorm)
        ));
        assert!(cosine_similarity(&raw, &raw).is_nan());
        assert_eq!(cosine_similarity(&raw, &[1.0, 0.0]).to_bits(), 0);
    }
    assert_eq!(control.memory().used(), 0);
    let control = StorageReadControl::with_limit(64);
    assert_eq!(
        navigable(&[1.0e-20, 0.0], &control).coordinates(),
        &[1.0, 0.0]
    );
}

#[test]
fn squared_navigation_uses_the_squared_euclidean_pruning_factor() {
    let control = StorageReadControl::with_limit(1024);
    let source = navigable(&[1.0, 0.0], &control);
    let near = navigable(&[3.0, 4.0], &control);
    let far = navigable(&[-1.0, 0.0], &control);
    let between = near.squared_distance(&far, &control).unwrap().get();
    let outer = source.squared_distance(&far, &control).unwrap().get();
    assert!((between - 3.2).abs() < 1.0e-15);
    assert_eq!(outer, 4.0);
    let alpha = DiskANNAlpha::new(1.2).unwrap();
    assert!(alpha.squared() * between > outer);
    assert!(alpha.get() * between < outer);
}

#[test]
fn invalid_inputs_fail_without_allocations_and_controls_release_workspace() {
    let control = StorageReadControl::with_limit(1024);
    for (dimensions, raw) in [
        (0, vec![]),
        (1, vec![1.0, 2.0]),
        (1, vec![f32::NAN]),
        (1, vec![f32::INFINITY]),
    ] {
        assert!(NavigationInput::from_raw(dimensions, &raw, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let left = navigable(&[1.0], &control);
    let right = navigable(&[1.0, 0.0], &control);
    assert!(left.squared_distance(&right, &control).is_err());
    for limit in 0..16 {
        let limited = StorageReadControl::with_limit(limit);
        assert!(matches!(
            NavigationInput::from_raw(2, &[3.0, 4.0], &limited),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(limited.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(matches!(
        NavigationInput::from_raw(1, &[1.0], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        left.squared_distance(&left, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
}

proptest! {
    #[test]
    fn navigation_distance_is_symmetric_nonnegative_and_zero_on_self(
        left in prop::array::uniform5(-1000_i16..=1000),
        right in prop::array::uniform5(-1000_i16..=1000),
    ) {
        prop_assume!(left.iter().any(|&x| x != 0) && right.iter().any(|&x| x != 0));
        let control = StorageReadControl::with_limit(1024);
        let left = navigable(&left.map(f32::from), &control);
        let right = navigable(&right.map(f32::from), &control);
        let forward = left.squared_distance(&right, &control).unwrap().get();
        let reverse = right.squared_distance(&left, &control).unwrap().get();
        prop_assert_eq!(forward.to_bits(), reverse.to_bits());
        prop_assert!((0.0..=4.000_000_000_000_002).contains(&forward));
        prop_assert_eq!(left.squared_distance(&left, &control).unwrap().get(), 0.0);
    }
}
