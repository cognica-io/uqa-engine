//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::key_value::SQLiteKeyValueStore;
use uqa_storage::{read_control::StorageReadControl, KeyValueStore, StorageBackendError};

#[test]
fn controlled_prefixes_preserve_binary_order_limits_and_exclusive_lower_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let store = SQLiteKeyValueStore::open(&directory.path().join("ordered.sqlite3")).unwrap();
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
    let control = StorageReadControl::with_limit(4096);
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
                assert_eq!(
                    actual, expected,
                    "prefix={prefix:?}, after={after:?}, limit={limit}"
                );
                assert_eq!(control.memory().used(), 7);
            }
        }
    }
    let mut missing = false;
    store
        .visit_value(b"missing", &control, &mut |value| {
            missing = value.is_none();
            Ok(())
        })
        .unwrap();
    assert!(missing);
    drop(other);
}

#[test]
fn prefix_existence_does_not_materialize_large_values() {
    let store = SQLiteKeyValueStore::open_in_memory().unwrap();
    store.put(b"abc", &vec![1; 65536]).unwrap();
    let control = StorageReadControl::with_limit(32);
    let other = control.memory().reserve(7).unwrap();
    for (prefix, expected) in [
        (b"".as_slice(), true),
        (b"ab", true),
        (b"ac", false),
        (b"\xff", false),
    ] {
        assert_eq!(
            store.contains_prefix_budgeted(prefix, &control).unwrap(),
            expected
        );
        assert_eq!(control.memory().used(), 7);
    }
    control.cancellation().cancel();
    assert!(matches!(
        store.contains_prefix_budgeted(b"ab", &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 7);
    drop(other);
}

#[test]
fn blob_reads_reserve_before_visiting_and_release_on_limits_or_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let store = SQLiteKeyValueStore::open(&directory.path().join("quota.sqlite3")).unwrap();
    store.put(b"key", &[17; 64]).unwrap();
    let baseline = StorageReadControl::with_limit(4096);
    store
        .visit_value(b"key", &baseline, &mut |value| {
            assert_eq!(value.unwrap(), [17; 64]);
            Ok(())
        })
        .unwrap();
    let peak = baseline.memory().peak();
    assert_eq!(peak, 67);
    for limit in 0..=peak {
        let control = StorageReadControl::with_limit(limit + 7);
        let other = control.memory().reserve(7).unwrap();
        let mut visited = false;
        let result = store.visit_value(b"key", &control, &mut |_| {
            visited = true;
            Ok(())
        });
        if limit == peak {
            result.unwrap();
            assert!(visited);
        } else {
            assert!(matches!(result, Err(StorageBackendError::Memory(_))));
            assert!(!visited);
        }
        assert_eq!(control.memory().used(), 7);
        drop(other);
    }
    let control = StorageReadControl::with_limit(4096);
    let other = control.memory().reserve(7).unwrap();
    let error = store
        .visit_value(b"key", &control, &mut |_| {
            control.cancellation().cancel();
            control.check()
        })
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert_eq!(control.memory().used(), 7);
    control.cancellation().reset();
    store
        .visit_value(b"key", &control, &mut |value| {
            assert_eq!(value.unwrap(), [17; 64]);
            Ok(())
        })
        .unwrap();
    drop(other);
}

#[test]
fn paged_size_probes_and_payloads_share_a_snapshot_during_concurrent_growth() {
    let directory = tempfile::tempdir().unwrap();
    let store = SQLiteKeyValueStore::open(&directory.path().join("snapshot.sqlite3")).unwrap();
    store
        .connection()
        .with(|connection| {
            connection.pragma_update(None, "journal_mode", "WAL")?;
            Ok(())
        })
        .unwrap();
    store.put(b"a", b"first").unwrap();
    store.put(b"b", &[1; 1024]).unwrap();
    let writer = store.new_session();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let write = std::thread::spawn(move || {
        start_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        writer.put(b"b", &vec![2; 65_536]).unwrap();
        done_tx.send(()).unwrap();
    });
    let control = StorageReadControl::with_limit(2048);
    let mut seen = 0;
    store
        .visit_prefix_after(b"", None, usize::MAX, &control, &mut |key, value| {
            if key == b"a" {
                start_tx.send(()).unwrap();
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                assert_eq!(value, b"first");
            } else {
                assert_eq!(key, b"b");
                assert_eq!(value, [1; 1024]);
            }
            seen += 1;
            Ok(())
        })
        .unwrap();
    write.join().unwrap();
    assert_eq!(seen, 2);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(store.get(b"b").unwrap().unwrap(), vec![2; 65_536]);
}
