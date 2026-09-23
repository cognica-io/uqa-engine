//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Custom provider pages retain their original allowance through both snapshot consumers.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::memory::{BudgetedVec, MemoryReservation};

struct ControlledSource {
    rows: MemoryDocumentStore,
    control: StorageReadControl,
    held: Mutex<Option<MemoryReservation>>,
    row_reads: AtomicUsize,
    exhaust: bool,
    forbid_borrowed: bool,
    cancel_on_return: Option<uqa_core::CancellationToken>,
}

impl ControlledSource {
    fn new(control: &StorageReadControl, exhaust: bool) -> Self {
        let mut rows = MemoryDocumentStore::new();
        for id in [1, 3] {
            rows.put_stored(
                id,
                document(&[("id", Value::Int(i64::try_from(id).unwrap()))], 42),
            )
            .unwrap();
        }
        Self {
            rows,
            control: control.clone(),
            held: Mutex::new(None),
            row_reads: AtomicUsize::new(0),
            exhaust,
            forbid_borrowed: false,
            cancel_on_return: None,
        }
    }
}

impl DocumentStore for ControlledSource {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable input")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable input")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable input")
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.row_reads.fetch_add(1, Ordering::Relaxed);
        self.rows.get_stored(id)
    }
    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        self.row_reads.fetch_add(ids.len(), Ordering::Relaxed);
        self.rows.get_stored_many(ids)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.rows.contains_doc_id(id)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("uncontrolled identity read")
    }
    fn next_doc_ids(&self, _: Option<DocId>, _: usize) -> StorageBackendResult<Vec<DocId>> {
        panic!("uncontrolled identity page")
    }
    fn for_each_next_fields(
        &self,
        _: Option<DocId>,
        _: usize,
        _: &[&str],
        _: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        assert!(
            !self.forbid_borrowed,
            "borrowed cursor cannot accept the invoking allowance"
        );
        Ok(None)
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        assert!(control.memory().shares_allowance(self.control.memory()));
        let ids = self.rows.next_doc_ids_controlled(after, limit, control)?;
        if self.exhaust && !ids.is_empty() {
            let available = control.memory().limit() - control.memory().used();
            *self.held.lock() = Some(
                control
                    .memory()
                    .reserve(available - (size_of::<DocId>() - 1))?,
            );
        }
        if let Some(cancel) = &self.cancel_on_return {
            cancel.cancel();
        }
        Ok(ids)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("capture must not replace the selected input")
    }
}

#[test]
fn explicit_controlled_pages_propagate_the_invoking_allowance_through_retained_views() {
    let capture_control = StorageReadControl::with_limit(128 << 10);
    let caller_control = StorageReadControl::with_limit(128);
    let mut source = ControlledSource::new(&caller_control, false);
    source.forbid_borrowed = true;
    let columns = columns("CREATE TABLE t (id INTEGER)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(source),
        &columns,
        &schema(&columns, &text),
        DocumentChanges::default(),
        &capture_control,
    )
    .unwrap();
    let retained = capture_control.memory().used();
    let ids = uqa_storage::document_store::read_document_ids(
        view.documents.as_ref(),
        None,
        2,
        &caller_control,
    )
    .unwrap();
    assert_eq!(&*ids, &[1, 3]);
    assert_eq!(capture_control.memory().used(), retained);
    assert_eq!(
        caller_control.memory().used(),
        ids.capacity() * size_of::<DocId>()
    );
    drop(ids);
    assert_eq!(caller_control.memory().used(), 0);
    capture_control.cancellation().cancel();
    assert!(matches!(
        view.documents
            .next_doc_ids_controlled(None, 0, &caller_control),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(view);
    assert_eq!(capture_control.memory().used(), 0);
}

#[test]
fn both_snapshot_consumers_keep_provider_identity_pages_charged_during_selection() {
    for copied in [false, true] {
        let control = StorageReadControl::with_limit(128 << 10);
        let source = Arc::new(ControlledSource::new(&control, true));
        let columns = columns("CREATE TABLE t (id INTEGER)");
        let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        let schema = schema(&columns, &text);
        if copied {
            let error = materialize(
                source.as_ref(),
                &columns,
                &schema,
                DocumentChanges::default(),
                &control,
            )
            .err()
            .expect("producer page and consumer selection coexist");
            assert_eq!(error.sqlstate(), Some("53200"));
        } else {
            let view = retain(
                source.clone(),
                &columns,
                &schema,
                DocumentChanges::default(),
                &control,
            )
            .unwrap();
            assert!(matches!(
                view.documents.next_doc_ids(None, 2),
                Err(StorageBackendError::Memory(_))
            ));
            drop(view);
        }
        assert_eq!(source.row_reads.load(Ordering::Relaxed), 0);
        drop(source.held.lock().take());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn both_snapshot_consumers_use_controlled_pages_and_keep_private_masks() {
    for copied in [false, true] {
        let control = StorageReadControl::with_limit(128 << 10);
        let source = Arc::new(ControlledSource::new(&control, false));
        let columns = columns("CREATE TABLE t (id INTEGER)");
        let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        let schema = schema(&columns, &text);
        let changes = DocumentChanges::from_rows(
            BTreeMap::from([
                (1, None),
                (2, Some(document(&[("id", Value::Int(20))], 43))),
            ]),
            &control,
        )
        .unwrap();
        let view = if copied {
            materialize(source.as_ref(), &columns, &schema, changes, &control)
        } else {
            retain(source.clone(), &columns, &schema, changes, &control)
        }
        .unwrap();
        assert_eq!(view.documents.doc_ids().unwrap(), [2, 3]);
        assert_eq!(
            view.documents
                .get_metadata(2)
                .unwrap()
                .unwrap()
                .tuple_xmin(),
            Some(43)
        );
        assert_eq!(
            source.row_reads.load(Ordering::Relaxed),
            usize::from(copied)
        );
        drop(view);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn retained_identity_selection_checks_captured_cancellation_before_allocating() {
    let capture = StorageReadControl::with_limit(128 << 10);
    let caller = StorageReadControl::with_limit(128 << 10);
    let mut source = ControlledSource::new(&caller, true);
    source.forbid_borrowed = true;
    source.cancel_on_return = Some(capture.cancellation().clone());
    let source = Arc::new(source);
    let columns = columns("CREATE TABLE t (id INTEGER)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        source.clone(),
        &columns,
        &schema(&columns, &text),
        DocumentChanges::default(),
        &capture,
    )
    .unwrap();
    assert!(matches!(
        uqa_storage::document_store::read_document_ids(view.documents.as_ref(), None, 2, &caller),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(source.row_reads.load(Ordering::Relaxed), 0);
    drop(source.held.lock().take());
    assert_eq!(caller.memory().used(), 0);
    drop(view);
    assert_eq!(capture.memory().used(), 0);
}
