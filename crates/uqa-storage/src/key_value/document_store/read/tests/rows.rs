//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    document_store::read_stored_documents,
    key_value::{KeyValueRead, KeyValueReadRevision},
    read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor},
    DocumentMetadata, StorageBackendResult, StoredDocument,
};
use std::cell::Cell;

struct Reader {
    control: StorageReadControl,
    bytes: Vec<u8>,
    calls: usize,
    attempts: Cell<usize>,
    cancel: bool,
    fail: bool,
}

impl KeyValueRead for Reader {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("revision");
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("prefix");
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("uncontrolled value read");
    }
    fn visit_value_budgeted(
        &self,
        _: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        for _ in 0..self.calls {
            self.attempts.set(self.attempts.get() + 1);
            let _ = visit(Some(&self.bytes));
        }
        if self.cancel {
            control.cancellation().cancel();
        }
        if self.fail {
            return Err(StorageBackendError::Other("cleanup failure".into()));
        }
        Ok(())
    }
}

#[test]
fn whole_row_decode_keeps_the_first_typed_failure_when_the_provider_ignores_it() {
    let row = StoredDocument::new([("large".into(), Value::Str("x".repeat(64 << 10)))].into());
    let reader = Reader {
        control: StorageReadControl::with_limit(1 << 20),
        bytes: crate::key_value::codec::encode_stored_document_value(&row).unwrap(),
        calls: 3,
        attempts: Cell::new(0),
        cancel: true,
        fail: true,
    };
    let control = StorageReadControl::with_limit(4096);
    let documents = super::super::Documents {
        read: &reader,
        table: "docs",
    };
    assert!(matches!(
        documents.retained_many_controlled(&[1], &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(reader.attempts.get(), 3);
    assert!(control.cancellation().is_cancelled());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(reader.control.memory().used(), 0);
}

#[test]
fn whole_row_decode_rejects_missing_repeated_and_cancelled_provider_callbacks() {
    for (calls, cancel) in [(0, false), (2, false), (1, true)] {
        let reader = Reader {
            control: StorageReadControl::with_limit(1 << 20),
            bytes:
                crate::key_value::codec::encode_stored_document_value(&StoredDocument::default())
                    .unwrap(),
            calls,
            attempts: Cell::new(0),
            cancel,
            fail: false,
        };
        let control = StorageReadControl::with_limit(1 << 20);
        let documents = super::super::Documents {
            read: &reader,
            table: "docs",
        };
        let result = documents.retained_many_controlled(&[1], &control);
        if cancel {
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        } else {
            assert!(matches!(result, Err(StorageBackendError::Other(_))));
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn controlled_key_value_rows_retain_the_selected_payloads_and_invoking_allowance() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store, "docs");
    let row = StoredDocument::with_metadata(
        [("bytes".into(), Value::Bytes(vec![5; 16 << 10]))].into(),
        DocumentMetadata::with_tuple_xmin(81),
    );
    documents.put_stored(3, row.clone()).unwrap();
    let snapshot = documents.snapshot().unwrap();
    documents.delete(3).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let page = read_stored_documents(snapshot.as_ref(), &[3, 99, 3], &control).unwrap();
    assert!(page[1].is_none());
    for index in [0, 2] {
        assert_eq!(page[index].as_ref().unwrap().fields(), row.fields());
        assert_eq!(page[index].as_ref().unwrap().metadata(), row.metadata());
    }
    assert!(control.memory().used() >= 32 << 10);
    drop(snapshot);
    assert_eq!(page[0].as_ref().unwrap().fields(), row.fields());
    drop(page);
    assert_eq!(control.memory().used(), 0);
    assert!(read_stored_documents(&documents, &[3], &control).unwrap()[0].is_none());
    let tiny = StorageReadControl::with_limit(128);
    let long_name =
        KeyValueDocumentStore::new(Arc::new(MemoryKeyValueStore::new()), "long".repeat(4096));
    assert!(matches!(
        read_stored_documents(&long_name, &[1], &tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    assert!(read_stored_documents(&long_name, &[], &tiny)
        .unwrap()
        .is_empty());
}
