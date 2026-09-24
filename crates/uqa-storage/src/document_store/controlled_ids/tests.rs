//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled pages cannot invoke legacy materializers or detach from their selected view and allowance.

use std::sync::Arc;

use super::*;
use crate::{MemoryDocumentStore, ReadOnlySnapshot, RetainedDocumentStoreBuilder, StoredDocument};
use uqa_core::{memory::MemoryBudget, Value};

struct Unsupported;

impl DocumentStore for Unsupported {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("document payload")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("unbounded identity materialization")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("unrequested count")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacement snapshot")
    }
}

#[test]
fn unsupported_controlled_reads_never_invoke_legacy_materialization() {
    let control = StorageReadControl::with_limit(0);
    assert!(matches!(
        read_document_ids(&Unsupported, None, 1, &control),
        Err(StorageBackendError::Other(_))
    ));
    assert!(read_document_ids(&Unsupported, Some(u64::MAX), 0, &control)
        .unwrap()
        .is_empty());
    assert!(Unsupported
        .next_doc_ids_controlled(None, 0, &control)
        .unwrap()
        .is_empty());
    control.cancellation().cancel();
    for limit in [0, 1] {
        assert!(matches!(
            read_document_ids(&Unsupported, None, limit, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
    }
    assert_eq!(control.memory().used(), 0);
}

#[derive(Clone, Copy)]
enum Fault {
    None,
    Foreign,
    TooMany,
    Repeated,
    Reversed,
    BeforeCursor,
    Cancel,
}

struct Faulty {
    fault: Fault,
    foreign: MemoryBudget,
}

impl DocumentStore for Faulty {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("document payload")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("unbounded identities")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("count")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("snapshot")
    }
    fn next_doc_ids_controlled(
        &self,
        _: Option<DocId>,
        _: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        let budget = if matches!(self.fault, Fault::Foreign) {
            &self.foreign
        } else {
            control.memory()
        };
        let mut ids = BudgetedVec::new(budget);
        ids.extend_from_slice(match self.fault {
            Fault::TooMany => &[6, 8, 9],
            Fault::Repeated => &[6, 6],
            Fault::Reversed => &[8, 6],
            Fault::BeforeCursor => &[5, 8],
            _ => &[6, 8],
        })?;
        if matches!(self.fault, Fault::Cancel) {
            control.cancellation().cancel();
        }
        Ok(ids)
    }
}

#[test]
fn controlled_pages_reject_foreign_allowances_invalid_ranges_and_late_cancellation() {
    for fault in [
        Fault::Foreign,
        Fault::TooMany,
        Fault::Repeated,
        Fault::Reversed,
        Fault::BeforeCursor,
        Fault::Cancel,
    ] {
        let control = StorageReadControl::with_limit(128);
        let source = Faulty {
            fault,
            foreign: MemoryBudget::new(128),
        };
        let result = read_document_ids(&source, Some(5), 2, &control);
        if matches!(fault, Fault::Cancel) {
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        } else {
            assert!(matches!(result, Err(StorageBackendError::Other(_))));
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(source.foreign.used(), 0);
    }
    let control = StorageReadControl::with_limit(16);
    let source = Faulty {
        fault: Fault::None,
        foreign: MemoryBudget::new(0),
    };
    let ids = read_document_ids(&source, Some(5), 2, &control).unwrap();
    assert_eq!(&*ids, &[6, 8]);
    assert_eq!(control.memory().used(), 16);
    drop(ids);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_and_retained_pages_preserve_boundaries_without_copying_payloads() {
    let mut source = MemoryDocumentStore::new();
    for id in [1, 5, u64::MAX] {
        source
            .put(
                id,
                [("body".into(), Value::Str("x".repeat(32 << 10)))].into(),
            )
            .unwrap();
    }
    let snapshot = source.snapshot().unwrap();
    let readonly = ReadOnlySnapshot::new(snapshot.clone());
    source.delete(5).unwrap();
    let owner = StorageReadControl::with_limit(256 << 10);
    let mut builder = RetainedDocumentStoreBuilder::new(&owner);
    for id in [1, 5, u64::MAX] {
        builder
            .add_document(id, snapshot.get_stored(id).unwrap().unwrap())
            .unwrap();
    }
    let retained = builder.finish().unwrap();
    let original = owner.memory().used();
    for view in [snapshot.as_ref(), &readonly, &retained] {
        let control = StorageReadControl::with_limit(64);
        let ids = read_document_ids(view, Some(1), 2, &control).unwrap();
        assert_eq!(&*ids, &[5, u64::MAX]);
        assert_eq!(control.memory().used(), 16);
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        assert!(matches!(
            read_document_ids(view, None, 1, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 64);
        drop(occupied);
        assert_eq!(control.memory().used(), 16);
        drop(ids);
        assert_eq!(control.memory().used(), 0);
        assert!(
            read_document_ids(view, Some(u64::MAX), usize::MAX, &control)
                .unwrap()
                .is_empty()
        );
        assert_eq!(owner.memory().used(), original);
        control.cancellation().cancel();
        assert!(matches!(
            read_document_ids(view, None, 0, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
    }
    let control = StorageReadControl::with_limit(8);
    assert!(matches!(
        read_document_ids(snapshot.as_ref(), None, 3, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    let ids = read_document_ids(&source, Some(1), 1, &control).unwrap();
    assert_eq!(&*ids, &[u64::MAX]);
}
