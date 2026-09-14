//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn controlled_occurrence_cursors_share_allowances_across_redb_transaction_modes() {
    use std::{collections::BTreeMap, sync::Arc};
    use uqa_storage::{
        clustered_postings::PostingReadCursor, InvertedIndex, KeyValueInvertedIndex, TokenTermKey,
    };
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("occurrences.redb")).unwrap();
    let store = Arc::new(provider.store());
    let mut index =
        KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
    index
        .try_add_documents(
            [0, 65536, u64::MAX]
                .into_iter()
                .map(|id| {
                    (
                        id,
                        BTreeMap::from([("body".into(), "alpha alpha beta".into())]),
                    )
                })
                .collect(),
        )
        .unwrap();
    let term = TokenTermKey::from_text("alpha");
    for mode in ["autocommit", "read", "write"] {
        match mode {
            "read" => store.begin_read_transaction().unwrap(),
            "write" => store.begin_transaction().unwrap(),
            _ => {}
        }
        let control = StorageReadControl::with_limit(1 << 20);
        let other = control.memory().reserve(7).unwrap();
        {
            let mut cursor = index
                .posting_read_cursor_key_budgeted("body", &term, &control)
                .unwrap();
            assert_eq!(cursor.doc_freq(), 3);
            assert_eq!(
                index
                    .field_stats_scalar_budgeted("body", &control)
                    .unwrap()
                    .total_docs,
                3
            );
            for id in [0, 65536, u64::MAX] {
                assert_eq!(cursor.advance_to(id).unwrap().unwrap().doc_id, id);
                let live = control.memory().used();
                let records = index
                    .get_occurrences_budgeted(id, "body", &term, &control)
                    .unwrap();
                assert_eq!(records.len(), 2);
                assert!(control.memory().used() > live);
                drop(records);
                assert_eq!(control.memory().used(), live);
            }
            control.cancellation().cancel();
            assert!(matches!(
                cursor.advance(),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(cursor.current().unwrap().doc_id, u64::MAX);
            control.cancellation().reset();
            assert_eq!(cursor.advance().unwrap(), None);
        }
        assert_eq!(control.memory().used(), 7);
        drop(other);
        if mode != "autocommit" {
            store.rollback_transaction().unwrap();
        }
    }
}

#[test]
fn borrowed_reads_preserve_binary_order_in_autocommit_and_retained_transactions() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("controlled.redb")).unwrap();
    let store = provider.store();
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
    for mode in ["autocommit", "read", "write"] {
        match mode {
            "read" => store.begin_read_transaction().unwrap(),
            "write" => store.begin_transaction().unwrap(),
            _ => {}
        }
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
                            key.starts_with(prefix)
                                && after.is_none_or(|after| key.as_slice() > after)
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
                    assert_eq!(actual, expected, "mode={mode}");
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
        control.cancellation().reset();
        if mode != "autocommit" {
            store.rollback_transaction().unwrap();
        }
    }
    drop(other);
    assert_eq!(control.memory().used(), 0);
}
