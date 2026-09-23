//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ControlledSource {
    rows: MemoryDocumentStore,
    control: StorageReadControl,
    calls: Mutex<Vec<DocId>>,
    address: AtomicUsize,
    failure: Option<DocId>,
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
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("whole-row output must use controlled provider production")
    }
    fn get_stored_many(
        &self,
        _: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        panic!("whole-row batch must use controlled provider production")
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::RetainedDocumentPage> {
        assert!(control.memory().shares_allowance(self.control.memory()));
        assert_eq!(ids.len(), 1);
        self.calls.lock().extend_from_slice(ids);
        if self.failure == Some(ids[0]) {
            return Err(uqa_storage::mvcc::VersionError::ReadConflict {
                dependency: 7,
                expected: None,
                actual: None,
            }
            .into_storage_error());
        }
        let page = self.rows.get_stored_many_controlled(ids, control)?;
        if let Some(Some(row)) = page.first() {
            if let Value::Str(body) = &row.fields()["body"] {
                self.address
                    .store(body.as_ptr() as usize, Ordering::Relaxed);
            }
        }
        Ok(page)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.rows.contains_doc_id(id)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("whole-row reads do not collect source identities")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("the selected source must not advance")
    }
}

#[test]
fn whole_row_outputs_share_one_allowance_across_base_private_and_duplicate_ids() {
    let columns = columns("CREATE TABLE t (body TEXT)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let control = StorageReadControl::with_limit(256 << 10);
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(1, document(&[("body", Value::Str("a".repeat(4096)))], 41))
        .unwrap();
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([
            (
                2,
                Some(document(&[("body", Value::Str("b".repeat(4096)))], 42)),
            ),
            (3, None),
        ]),
        &control,
    )
    .unwrap();
    let view = retain(
        source.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        changes,
        &control,
    )
    .unwrap();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained - (6 << 10))
        .unwrap();
    let nested = view.documents.snapshot().unwrap();
    for rows in [view.documents.as_ref(), nested.as_ref()] {
        for id in [1, 2] {
            let row = rows.get_stored(id).unwrap().unwrap();
            assert_eq!(
                row.metadata().tuple_xmin(),
                Some(u32::try_from(id + 40).unwrap())
            );
            assert_eq!(control.memory().used(), retained + occupied.bytes());
            assert_eq!(rows.get_stored_many(&[id, id]).unwrap().len(), 1);
        }
        for ids in [&[1, 2][..], &[2, 1, 2, 3, 99][..]] {
            let error = rows.get_stored_many(ids).unwrap_err();
            assert_eq!(
                snapshot_error("owned provider batch", &error).sqlstate(),
                Some("53200")
            );
            assert_eq!(control.memory().used(), retained + occupied.bytes());
        }
    }
    drop(occupied);
    assert_eq!(nested.get_stored_many(&[2, 1, 2, 3, 99]).unwrap().len(), 2);
    assert_eq!(control.memory().used(), retained);
    control.cancellation().cancel();
    let error = nested.get_stored_many(&[1, 2]).unwrap_err();
    assert_eq!(
        snapshot_error("owned provider batch", &error).sqlstate(),
        Some("57014")
    );
    drop((view, nested));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn owned_rows_transfer_controlled_payloads_and_preserve_private_nulls_and_tuple_metadata() {
    let columns = columns("CREATE TABLE t (body TEXT)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let control = control();
    let mut rows = MemoryDocumentStore::new();
    for id in 1..=3 {
        rows.put_stored(
            id,
            document(&[("body", Value::Str("base".repeat(1024)))], 41),
        )
        .unwrap();
    }
    let source = Arc::new(ControlledSource {
        rows,
        control: control.clone(),
        calls: Mutex::default(),
        address: AtomicUsize::new(0),
        failure: None,
    });
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([(2, Some(document(&[("body", Value::Null)], 42))), (3, None)]),
        &control,
    )
    .unwrap();
    let view = retain(
        source.clone(),
        &columns,
        &schema(&columns, &index),
        changes,
        &control,
    )
    .unwrap();
    let nested = view.documents.snapshot().unwrap();
    let baseline = control.memory().used();
    let rows = nested.get_stored_many(&[2, 1, 2, 3, 99]).unwrap();
    assert_eq!(&*source.calls.lock(), &[1, 99]);
    assert_eq!(rows.keys().copied().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(rows[&1].metadata().tuple_xmin(), Some(41));
    assert_eq!(rows[&2], document(&[("body", Value::Null)], 42));
    let Value::Str(body) = &rows[&1].fields()["body"] else {
        panic!("base string");
    };
    assert_eq!(
        body.as_ptr() as usize,
        source.address.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), baseline);
    drop((view, nested));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn later_owned_provider_failure_releases_earlier_output_and_keeps_the_selected_view() {
    let columns = columns("CREATE TABLE t (body TEXT)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let control = control();
    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(
        1,
        document(&[("body", Value::Str("first".repeat(1024)))], 41),
    )
    .unwrap();
    rows.put_stored(2, document(&[("body", Value::Null)], 42))
        .unwrap();
    let source = Arc::new(ControlledSource {
        rows,
        control: control.clone(),
        calls: Mutex::default(),
        address: AtomicUsize::new(0),
        failure: Some(2),
    });
    let view = retain(
        source.clone(),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let baseline = control.memory().used();
    let error = view.documents.get_stored_many(&[1, 2, 1]).unwrap_err();
    assert_eq!(
        snapshot_error("owned provider", &error).sqlstate(),
        Some("40001")
    );
    assert_eq!(&*source.calls.lock(), &[1, 2]);
    assert_eq!(control.memory().used(), baseline);
    assert_eq!(
        view.documents
            .get_stored(1)
            .unwrap()
            .unwrap()
            .metadata()
            .tuple_xmin(),
        Some(41)
    );
    assert_eq!(control.memory().used(), baseline);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
