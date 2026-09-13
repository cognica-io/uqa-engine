//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::read_control::StorageReadControl;

#[test]
fn memory_visitors_borrow_binary_values_with_a_full_allowance() {
    let store = MemoryKeyValueStore::new();
    for key in [
        b"".as_slice(),
        b"a",
        b"a\0",
        b"a\xff",
        b"b",
        b"\xff",
        b"\xff\0",
        b"\xff\xff",
    ] {
        store.put(key, key).unwrap();
    }
    let all = store.scan_prefix(b"").unwrap();
    let control = StorageReadControl::with_limit(7);
    let other = control.memory().reserve(7).unwrap();
    for prefix in [b"".as_slice(), b"a", b"\xff", b"absent"] {
        for after in [
            None,
            Some(b"".as_slice()),
            Some(b"a".as_slice()),
            Some(b"a\0".as_slice()),
            Some(b"z".as_slice()),
            Some(b"\xff\xff".as_slice()),
        ] {
            for limit in [0, 1, 2, 16] {
                let expected: Vec<_> = all
                    .iter()
                    .filter(|(key, _)| {
                        key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after)
                    })
                    .take(limit)
                    .cloned()
                    .collect();
                let mut actual = Vec::new();
                store
                    .visit_prefix_after(prefix, after, limit, &control, &mut |key, value| {
                        actual.push((key.to_vec(), value.to_vec()));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(actual, expected);
                assert_eq!(control.memory().used(), 7);
            }
        }
    }
    for key in [b"a".as_slice(), b"missing"] {
        let expected = store.get(key).unwrap();
        store
            .visit_value(key, &control, &mut |value| {
                assert_eq!(value, expected.as_deref());
                Ok(())
            })
            .unwrap();
        let result = store.visit_value(key, &control, &mut |_| {
            control.cancellation().cancel();
            Ok(())
        });
        assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        control.cancellation().reset();
    }
    let mut calls = 0;
    let result = store.visit_prefix_after(b"", None, usize::MAX, &control, &mut |_, _| {
        calls += 1;
        control.cancellation().cancel();
        Ok(())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 7);
    drop(other);
}
