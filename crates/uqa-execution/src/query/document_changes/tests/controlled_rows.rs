//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::memory::MemoryReservation;
use uqa_storage::{RetainedDocumentPage, RetainedStoredDocument};

struct ControlledSource {
    rows: MemoryDocumentStore,
    returned: Mutex<Vec<RetainedStoredDocument>>,
    calls: AtomicUsize,
    fail_at: Option<usize>,
    retain_returned: bool,
    exhaust_after_read: bool,
    occupied: Mutex<Option<MemoryReservation>>,
}

impl ControlledSource {
    fn new() -> Self {
        let mut rows = MemoryDocumentStore::new();
        rows.put_stored(1, document(10)).unwrap();
        rows.put_stored(3, document(30)).unwrap();
        Self {
            rows,
            returned: Mutex::default(),
            calls: AtomicUsize::new(0),
            fail_at: None,
            retain_returned: false,
            exhaust_after_read: false,
            occupied: Mutex::default(),
        }
    }
}

impl DocumentStore for ControlledSource {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("uncontrolled row materialization");
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.rows.contains_doc_id(id)
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("corpus enumeration");
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("count");
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacement snapshot");
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDocumentPage> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if self.fail_at == Some(call) {
            return Err(uqa_core::memory::MemoryError::SizeOverflow.into());
        }
        let page = self.rows.get_stored_many_controlled(ids, control)?;
        if self.retain_returned {
            self.returned.lock().extend(page.iter().flatten().cloned());
        }
        if self.exhaust_after_read {
            *self.occupied.lock() = Some(
                control
                    .memory()
                    .reserve(control.memory().limit() - control.memory().used() - 8)?,
            );
        }
        Ok(page)
    }
}

#[test]
fn controlled_private_pages_share_owned_fields_and_admit_retained_provider_rows() {
    let owner = control();
    let caller = control();
    let source = Arc::new(ControlledSource::new());
    let changes =
        DocumentChanges::from_rows(BTreeMap::from([(1, Some(document(10))), (2, None)]), &owner)
            .unwrap()
            .with_retained(source.clone(), selection([(3, true)]), &owner)
            .unwrap();
    let page =
        uqa_storage::document_store::read_stored_documents(&changes, &[1, 3, 99, 1, 2, 3], &caller)
            .unwrap();
    for (position, expected) in [
        Some(document(10)),
        Some(document(30)),
        None,
        Some(document(10)),
        None,
        Some(document(30)),
    ]
    .iter()
    .enumerate()
    {
        match (&page[position], expected) {
            (Some(row), Some(expected)) => {
                assert_eq!(row.fields(), expected.fields());
                assert_eq!(row.metadata(), expected.metadata());
            }
            (None, None) => {}
            _ => panic!("private selection changed at {position}"),
        }
    }
    assert!(std::ptr::eq(
        page[0].as_ref().unwrap().fields(),
        page[3].as_ref().unwrap().fields()
    ));
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
    let empty = StorageReadControl::with_limit(0);
    assert!(
        uqa_storage::document_store::read_stored_documents(&changes, &[], &empty)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        uqa_storage::document_store::read_stored_documents(&changes, &[1, 3], &empty),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
    caller.cancellation().cancel();
    assert!(matches!(
        uqa_storage::document_store::read_stored_documents(&changes, &[1], &caller),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(changes);
    assert_eq!(owner.memory().used(), 0);
    assert!(caller.memory().used() > 0);
    drop(page);
    assert_eq!(caller.memory().used(), 0);
}

#[test]
fn copied_private_capture_transfers_provider_fields_without_copying_or_recharging_them() {
    let mut source = ControlledSource::new();
    source.retain_returned = true;
    let control = control();
    let desired = selection([(1, true), (2, false), (3, true), (9, true)]);
    let changes = DocumentChanges::capture_owned(&source, desired, &control).unwrap();
    assert_eq!(changes.doc_ids().unwrap(), [1, 3]);
    assert_eq!(source.calls.load(Ordering::Relaxed), 1);
    let returned = source.returned.lock();
    for (id, row) in [1, 3].into_iter().zip(returned.iter()) {
        let Some(Change::Fields(fields, metadata)) = changes.get(id) else {
            panic!("retained copied fields");
        };
        assert!(std::ptr::eq(fields.as_ref(), row.fields()));
        assert_eq!(*metadata, row.metadata());
    }
    drop(returned);
    let snapshot = changes.snapshot().unwrap();
    source.rows.clear().unwrap();
    source.returned.lock().clear();
    drop(changes);
    drop(source);
    assert_eq!(snapshot.get_stored(1).unwrap(), Some(document(10)));
    assert_eq!(snapshot.get_stored(3).unwrap(), Some(document(30)));
    assert!(control.memory().used() > 0);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn copied_private_capture_keeps_the_provider_lease_during_selection_failure() {
    let mut source = ControlledSource::new();
    source.exhaust_after_read = true;
    let control = control();
    assert!(matches!(
        DocumentChanges::capture_owned(&source, selection([(1, true)]), &control),
        Err(StorageBackendError::Memory(_))
    ));
    let occupied = source.occupied.lock().take().unwrap();
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.rows.get_stored(1).unwrap(), Some(document(10)));
}

#[test]
fn later_controlled_page_failure_releases_already_captured_private_payloads() {
    let mut source = ControlledSource::new();
    source.fail_at = Some(2);
    let control = control();
    let count = u64::try_from(crate::DEFAULT_BATCH_SIZE * 2 + 1).unwrap();
    let desired = selection((0..count).map(|id| (id, true)));
    let error = DocumentChanges::capture_owned(&source, desired, &control)
        .err()
        .unwrap();
    assert!(matches!(error, StorageBackendError::Memory(_)));
    assert_eq!(
        crate::storage_errors::storage_error("capture", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.rows.get_stored(1).unwrap(), Some(document(10)));
}
