//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::MemoryBudget;

#[test]
fn admitted_shared_values_keep_original_bytes_until_the_last_reader_retires() {
    #[derive(Clone)]
    struct NativeValue {
        data: Arc<Budgeted<Vec<u8>>>,
        revision: u8,
    }
    let input = MemoryBudget::new(4096);
    let payload = input.reserve(32).unwrap();
    let data = Budgeted::new(vec![7_u8; 32], payload)
        .into_shared()
        .unwrap();
    let payload_bytes = input.used();
    let mut current =
        RetainedValue::with_budget(NativeValue { data, revision: 7 }, &input).unwrap();
    let original = current.clone();
    let bytes = payload_bytes + size_of::<Budgeted<Admitted<NativeValue>>>();
    assert_eq!(input.used(), bytes);
    assert!(RetainedValue::ptr_eq(&current, &original));
    let exhausted = input.reserve(input.limit() - bytes).unwrap();
    RetainedValue::make_mut(&mut current).revision = 8;
    assert_eq!(original.revision, 7);
    assert_eq!(current.revision, 8);
    assert!(Arc::ptr_eq(&original.data, &current.data));
    assert!(!RetainedValue::ptr_eq(&current, &original));
    drop(exhausted);
    assert_eq!(input.used(), bytes);
    let reader = original.clone();
    drop(original);
    assert_eq!(input.used(), bytes);
    assert_eq!(reader.data.as_slice(), &[7; 32]);
    drop(reader);
    assert_eq!(input.used(), payload_bytes);
    assert_eq!(current.data.as_slice(), &[7; 32]);
    drop(current);
    assert_eq!(input.used(), 0);
}

#[test]
fn native_shared_values_preserve_identity_and_detach_only_shared_writes() {
    let native = Arc::new(vec![7]);
    let mut current = RetainedValue::from(native.clone());
    let saved = RetainedValue::from(native.clone());
    assert!(RetainedValue::ptr_eq(&current, &saved));
    RetainedValue::make_mut(&mut current).push(8);
    assert_eq!(&**native, &[7]);
    assert_eq!(&**saved, &[7]);
    assert_eq!(&**current, &[7, 8]);
    assert!(!RetainedValue::ptr_eq(&current, &saved));
    let pointer = std::ptr::from_ref(current.as_ref());
    RetainedValue::make_mut(&mut current)[0] = 9;
    assert_eq!(std::ptr::from_ref(current.as_ref()), pointer);
}

#[test]
fn denied_shared_header_retires_the_consumed_payload_with_its_reservation() {
    let input = MemoryBudget::new(32 + size_of::<Budgeted<Admitted<Budgeted<Vec<u8>>>>>() - 1);
    let payload = input.reserve(32).unwrap();
    assert!(matches!(
        RetainedValue::with_budget(Budgeted::new(vec![7_u8; 32], payload), &input),
        Err(MemoryError::Limit { .. })
    ));
    assert_eq!(input.used(), 0);
}

#[test]
fn weak_value_identity_does_not_retain_admitted_bytes_or_match_reused_allocations() {
    let input = MemoryBudget::new(4096);
    let value = RetainedValue::with_budget(7_u64, &input).unwrap();
    let identity = value.identity();
    let saved = value.clone();
    assert!(identity.matches(&saved));
    assert!(identity.is_alive());
    drop(value);
    assert!(input.used() > 0);
    drop(saved);
    assert_eq!(input.used(), 0);
    assert!(!identity.is_alive());
    for _ in 0..64 {
        let next = RetainedValue::with_budget(7_u64, &input).unwrap();
        assert!(!identity.matches(&next));
    }
    assert_eq!(input.used(), 0);
    let native = Arc::new(7_u64);
    let first = RetainedValue::from(native.clone());
    let identity = first.identity();
    assert!(identity.matches(&RetainedValue::from(native)));
    assert!(!identity.matches(&RetainedValue::new(7)));
}

#[test]
fn equal_shared_generations_preserve_constant_work_for_dependency_comparisons() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counted {
        value: u64,
        comparisons: Arc<AtomicUsize>,
    }
    impl PartialEq for Counted {
        fn eq(&self, other: &Self) -> bool {
            self.comparisons.fetch_add(1, Ordering::SeqCst);
            self.value == other.value
        }
    }
    impl Eq for Counted {}
    let comparisons = Arc::new(AtomicUsize::new(0));
    let input = MemoryBudget::new(4096);
    for admitted in [false, true] {
        let value = Counted {
            value: 7,
            comparisons: comparisons.clone(),
        };
        let original = if admitted {
            RetainedValue::with_budget(value, &input).unwrap()
        } else {
            RetainedValue::new(value)
        };
        let saved = original.clone();
        comparisons.store(0, Ordering::SeqCst);
        assert!(original == saved);
        assert_eq!(comparisons.load(Ordering::SeqCst), 0);
        let equal = RetainedValue::new(Counted {
            value: 7,
            comparisons: comparisons.clone(),
        });
        assert!(original == equal);
        assert_eq!(comparisons.load(Ordering::SeqCst), 1);
    }
    assert_eq!(input.used(), 0);
}
