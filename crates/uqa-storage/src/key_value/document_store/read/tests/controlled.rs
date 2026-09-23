//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invoking query allowances cover key cursors and output without loading document values.

use super::*;
use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::read_control::{KeyReadVisitor, KeyValueReadVisitor, ValueReadVisitor};
use crate::StorageBackendResult;
use crate::{document_store::read_document_ids, read_control::StorageReadControl};
use std::cell::Cell;

struct FaultyRead {
    control: StorageReadControl,
    keys: Vec<Vec<u8>>,
    attempts: Cell<usize>,
    cancel_at_end: bool,
    fail_at_end: bool,
}

impl KeyValueRead for FaultyRead {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("identity enumeration does not need a revision")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("identity enumeration must not read values")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("identity enumeration must not read values")
    }
    fn visit_keys_after(
        &self,
        _: &[u8],
        _: Option<&[u8]>,
        _: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        for key in &self.keys {
            self.attempts.set(self.attempts.get() + 1);
            // Deliberately suppress visitor errors and keep calling it to test admission failure retention.
            let _ = visit(key);
        }
        if self.cancel_at_end {
            control.cancellation().cancel();
        }
        if self.fail_at_end {
            Err(StorageBackendError::Other("scanner cleanup failed".into()))
        } else {
            Ok(())
        }
    }
}

fn reader(keys: Vec<Vec<u8>>) -> FaultyRead {
    FaultyRead {
        control: StorageReadControl::with_limit(4096),
        keys,
        attempts: Cell::new(0),
        cancel_at_end: false,
        fail_at_end: false,
    }
}

#[test]
fn identity_scan_keeps_its_first_quota_failure_through_later_callbacks_and_cleanup() {
    let keys = [1, 2, 3]
        .into_iter()
        .map(|id| crate::key_value::codec::document_key("docs", id).unwrap())
        .collect();
    let mut read = reader(keys);
    read.cancel_at_end = true;
    read.fail_at_end = true;
    // Prefix + cursor + first identity fit; growing the result does not.
    let control = StorageReadControl::with_limit(9 + 17 + size_of::<u64>());
    let documents = super::super::Documents {
        read: &read,
        table: "docs",
    };
    assert!(matches!(
        documents.id_page_controlled(Some(0), 3, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(read.attempts.get(), 3);
    assert!(control.cancellation().is_cancelled());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(read.control.memory().used(), 0);
}

#[test]
fn identity_scan_rejects_malformed_ranges_and_cancellation_after_its_last_key() {
    let key = |table, id| crate::key_value::codec::document_key(table, id).unwrap();
    for keys in [
        vec![key("other", 1)],
        vec![vec![b'd']],
        vec![key("docs", 1), key("docs", 1)],
        vec![key("docs", 2), key("docs", 1)],
        vec![key("docs", 0)],
        vec![key("docs", 1), key("docs", 2), key("docs", 3)],
    ] {
        let read = reader(keys);
        let control = StorageReadControl::with_limit(4096);
        let documents = super::super::Documents {
            read: &read,
            table: "docs",
        };
        assert!(matches!(
            documents.id_page_controlled(Some(0), 2, &control),
            Err(StorageBackendError::Other(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    let mut read = reader(vec![key("docs", 1)]);
    read.cancel_at_end = true;
    let control = StorageReadControl::with_limit(4096);
    let documents = super::super::Documents {
        read: &read,
        table: "docs",
    };
    assert!(matches!(
        documents.id_page_controlled(None, 2, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_value_identity_pages_use_the_invoking_allowance_and_keep_the_captured_view() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store.clone(), "docs");
    for id in [1, 3, 5] {
        documents
            .put(
                id,
                [("opaque".into(), Value::Str("x".repeat(128 << 10)))].into(),
            )
            .unwrap();
    }
    let retained = documents.snapshot().unwrap();
    documents.delete(3).unwrap();
    documents.put(4, BTreeMap::new()).unwrap();
    let control = StorageReadControl::with_limit(128);
    for (source, expected) in [
        (&documents as &dyn DocumentStore, [4, 5]),
        (retained.as_ref(), [3, 5]),
    ] {
        let ids = read_document_ids(source, Some(1), 2, &control).unwrap();
        assert_eq!(&*ids, &expected);
        assert_eq!(control.memory().used(), ids.capacity() * size_of::<u64>());
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        assert!(matches!(
            read_document_ids(source, None, 1, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(read_document_ids(source, None, 0, &control)
            .unwrap()
            .is_empty());
        drop(occupied);
        drop(ids);
        assert_eq!(control.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(matches!(
        read_document_ids(retained.as_ref(), None, 0, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_value_cursor_prefix_is_reserved_before_copying_a_large_table_name() {
    let table = "t".repeat(16 << 10);
    let documents = KeyValueDocumentStore::new(Arc::new(MemoryKeyValueStore::new()), table);
    let control = StorageReadControl::with_limit(64);
    assert!(matches!(
        read_document_ids(&documents, None, 1, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(control.memory().peak(), 0);
    assert!(read_document_ids(&documents, None, 0, &control)
        .unwrap()
        .is_empty());
}
