//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::{RetainedDocumentPage, RetainedStoredDocument};

struct Source {
    rows: MemoryDocumentStore,
    address: AtomicUsize,
    reads: AtomicUsize,
    held: Mutex<Option<MemoryReservation>>,
    shared: Mutex<Option<RetainedStoredDocument>>,
    exhaust: bool,
    fail_on_read: Option<usize>,
}

impl Source {
    fn new() -> Self {
        let mut rows = MemoryDocumentStore::new();
        rows.put_stored(1, document(&[("body", Value::Str("original".into()))], 41))
            .unwrap();
        Self {
            rows,
            address: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            held: Mutex::new(None),
            shared: Mutex::new(None),
            exhaust: false,
            fail_on_read: None,
        }
    }
}

impl DocumentStore for Source {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("read-only input")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("read-only input")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("read-only input")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("uncontrolled row materialization")
    }
    fn get_stored_many(
        &self,
        _: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        panic!("uncontrolled bulk materialization")
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDocumentPage> {
        let call = self.reads.fetch_add(1, Ordering::Relaxed) + 1;
        if self.fail_on_read == Some(call) {
            return Err(uqa_core::QueryCancelled.into());
        }
        let mut page = self.rows.get_stored_many_controlled(ids, control)?;
        if let Some(Some(row)) = page.first() {
            let Value::Str(text) = &row.fields()["body"] else {
                panic!("text payload");
            };
            self.address
                .store(text.as_ptr() as usize, Ordering::Relaxed);
        }
        if self.exhaust {
            page.reserve(256)?;
            *self.shared.lock() = page.first().cloned().flatten();
            let available = control.memory().limit() - control.memory().used();
            *self.held.lock() = Some(control.memory().reserve(available)?);
        }
        Ok(page)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("uncontrolled identity materialization")
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.rows.next_doc_ids_controlled(after, limit, control)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("legacy snapshots cannot certify immutable retention")
    }
}

#[test]
fn copied_corpus_adopts_controlled_payloads_without_recopying_their_values() {
    let control = control();
    let source = Source::new();
    let columns = columns("CREATE TABLE t (body TEXT)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = materialize(
        &source,
        &columns,
        &schema(&columns, &text),
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let address = source.address.load(Ordering::Relaxed);
    assert_ne!(address, 0);
    assert_eq!(source.reads.load(Ordering::Relaxed), 1);
    drop(source);
    view.documents
        .for_each_fields_multi_ref(&[1], &["body"], &mut |_, values| {
            let Value::Str(value) = values[0] else {
                panic!("text payload");
            };
            assert_eq!(value.as_ptr() as usize, address);
            true
        })
        .unwrap();
    assert_eq!(
        view.documents
            .get_metadata(1)
            .unwrap()
            .unwrap()
            .tuple_xmin(),
        Some(41)
    );
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn copied_corpus_keeps_the_provider_page_charged_during_shared_payload_adoption() {
    let control = control();
    let mut source = Source::new();
    source.exhaust = true;
    let columns = columns("CREATE TABLE t (body TEXT)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let error = materialize(
        &source,
        &columns,
        &schema(&columns, &text),
        DocumentChanges::default(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(source.reads.load(Ordering::Relaxed), 1);
    assert!(source.shared.lock().is_some());
    drop(source.held.lock().take());
    assert!(control.memory().used() > 0);
    drop(source.shared.lock().take());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        source.rows.get_field(1, "body").unwrap(),
        Some(Value::Str("original".into()))
    );
}

#[test]
fn later_copied_page_failure_discards_new_rows_and_preserves_the_previous_view() {
    let control = StorageReadControl::with_limit(4 << 20);
    let mut source = Source::new();
    let columns = columns("CREATE TABLE t (body TEXT)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &text);
    let original = materialize(
        &source,
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let used = control.memory().used();
    for id in 2..=u64::try_from(crate::DEFAULT_BATCH_SIZE + 2).unwrap() {
        source
            .rows
            .put_stored(id, document(&[("body", Value::Str("later".into()))], 42))
            .unwrap();
    }
    source.reads.store(0, Ordering::Relaxed);
    source.fail_on_read = Some(2);
    let error = materialize(
        &source,
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(source.reads.load(Ordering::Relaxed), 2);
    assert_eq!(control.memory().used(), used);
    assert_eq!(original.documents.doc_ids().unwrap(), [1]);
    assert_eq!(
        original.documents.get_field(1, "body").unwrap(),
        Some(Value::Str("original".into()))
    );
    drop(original);
    assert_eq!(control.memory().used(), 0);
}
