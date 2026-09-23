//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uqa_core::memory::BudgetedVec;

type CursorRequest = (Option<DocId>, usize);

#[derive(Clone, Copy)]
enum Fault {
    None,
    Repeated,
    Reversed,
    BeforeCursor,
    OverLimit,
    WrongCount,
    UnsupportedAfterCallback,
}

#[derive(Clone)]
struct BorrowedSource {
    rows: Arc<MemoryDocumentStore>,
    control: StorageReadControl,
    requests: Arc<Mutex<Vec<CursorRequest>>>,
    visited: Arc<AtomicUsize>,
    exhaust: Arc<AtomicBool>,
    cancel_after_stop: bool,
    fault: Fault,
}

impl BorrowedSource {
    fn new(control: &StorageReadControl) -> Self {
        let mut rows = MemoryDocumentStore::new();
        for id in [1, 3, 5, 9] {
            rows.put_stored(id, document(&[("key", Value::Int(10))], 41))
                .unwrap();
        }
        Self {
            rows: Arc::new(rows),
            control: control.clone(),
            requests: Arc::default(),
            visited: Arc::default(),
            exhaust: Arc::default(),
            cancel_after_stop: false,
            fault: Fault::None,
        }
    }
}

impl DocumentStore for BorrowedSource {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.rows.get_stored(id)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.rows.contains_doc_id(id)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("retained views must use a supported borrowed cursor")
    }
    fn next_doc_ids(&self, _: Option<DocId>, _: usize) -> StorageBackendResult<Vec<DocId>> {
        panic!("retained views must not detach a supported provider page from its lease")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        assert!(fields.is_empty());
        self.control.check()?;
        self.requests.lock().push((after, limit));
        let mut ids = BudgetedVec::new(self.control.memory());
        for id in [1, 3, 5, 9]
            .into_iter()
            .filter(|id| after.is_none_or(|after| *id > after))
            .take(limit)
        {
            ids.push(id)?;
        }
        match self.fault {
            Fault::Repeated if ids.len() > 1 => ids[1] = ids[0],
            Fault::Reversed => ids.reverse(),
            Fault::BeforeCursor if !ids.is_empty() => ids[0] = after.unwrap_or(0),
            Fault::OverLimit => ids.push(11)?,
            _ => {}
        }
        let _scratch = self.exhaust.load(Ordering::Relaxed).then(|| {
            self.control
                .memory()
                .reserve(self.control.memory().limit() - self.control.memory().used())
                .unwrap()
        });
        let mut count = 0;
        for id in ids.iter().copied() {
            self.visited.fetch_add(1, Ordering::Relaxed);
            count += 1;
            if !visitor(id, &[]) {
                if self.cancel_after_stop {
                    self.control.cancellation().cancel();
                }
                break;
            }
        }
        self.control.check()?;
        Ok(match self.fault {
            Fault::WrongCount => Some(count + 1),
            Fault::UnsupportedAfterCallback => None,
            _ => Some(count),
        })
    }
}

fn capture(source: &BorrowedSource, changes: DocumentChanges) -> MaterializedTable {
    let columns = columns("CREATE TABLE t (key INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    retain(
        Arc::new(source.clone()),
        &columns,
        &schema(&columns, &index),
        changes,
        &source.control,
    )
    .unwrap()
}

#[test]
fn borrowed_identity_cursors_preserve_private_masks_and_ordered_advancement() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let source = BorrowedSource::new(&control);
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([
            (2, Some(document(&[("key", Value::Int(20))], 42))),
            (3, None),
            (5, Some(document(&[("key", Value::Int(50))], 42))),
            (7, Some(document(&[("key", Value::Int(70))], 42))),
        ]),
        &control,
    )
    .unwrap();
    let view = capture(&source, changes);
    let retained = control.memory().used();
    assert_eq!(view.documents.next_doc_ids(None, 2).unwrap(), [1, 2]);
    assert_eq!(view.documents.next_doc_ids(Some(2), 2).unwrap(), [5, 7]);
    assert_eq!(view.documents.next_doc_ids(Some(7), 2).unwrap(), [9]);
    assert!(view.documents.next_doc_ids(Some(9), 2).unwrap().is_empty());
    assert!(source.requests.lock().iter().all(|(_, limit)| *limit <= 2));
    let nested = view.documents.snapshot().unwrap();
    assert_eq!(nested.doc_ids().unwrap(), [1, 2, 5, 7, 9]);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        view.documents
            .get_metadata(5)
            .unwrap()
            .unwrap()
            .tuple_xmin(),
        Some(42)
    );
    drop(nested);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn borrowed_identity_input_keeps_its_original_allowance_and_first_error() {
    for cancel_after_stop in [false, true] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut source = BorrowedSource::new(&control);
        source.cancel_after_stop = cancel_after_stop;
        let view = capture(&source, DocumentChanges::default());
        let retained = control.memory().used();
        source.exhaust.store(true, Ordering::Relaxed);
        assert!(matches!(
            view.documents.next_doc_ids(None, 2),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(source.visited.load(Ordering::Relaxed), 1);
        assert_eq!(control.memory().used(), retained);
        source.exhaust.store(false, Ordering::Relaxed);
        if cancel_after_stop {
            assert!(matches!(
                view.documents.next_doc_ids(None, 0),
                Err(StorageBackendError::Cancelled(_))
            ));
        } else {
            assert_eq!(view.documents.next_doc_ids(None, 4).unwrap(), [1, 3, 5, 9]);
            assert_eq!(control.memory().used(), retained);
        }
        drop(view);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn malformed_borrowed_identity_cursors_release_partial_consumer_buffers() {
    for fault in [
        Fault::Repeated,
        Fault::Reversed,
        Fault::BeforeCursor,
        Fault::OverLimit,
        Fault::WrongCount,
        Fault::UnsupportedAfterCallback,
    ] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut source = BorrowedSource::new(&control);
        let prior = capture(&source, DocumentChanges::default());
        source.fault = fault;
        let view = capture(&source, DocumentChanges::default());
        let retained = control.memory().used();
        assert!(matches!(
            view.documents.next_doc_ids(Some(0), 2),
            Err(StorageBackendError::Other(_))
        ));
        assert_eq!(control.memory().used(), retained);
        assert_eq!(prior.documents.next_doc_ids(None, 4).unwrap(), [1, 3, 5, 9]);
        assert_eq!(control.memory().used(), retained);
        drop(view);
        drop(prior);
        assert_eq!(control.memory().used(), 0);
    }
}
