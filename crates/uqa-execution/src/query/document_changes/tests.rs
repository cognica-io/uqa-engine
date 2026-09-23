//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use uqa_core::CancellationToken;
use uqa_storage::MemoryDocumentStore;

mod budgets;
mod controlled_rows;
mod projection;

#[derive(Clone)]
struct Probe {
    source: Arc<dyn DocumentStore>,
    projections: Arc<Mutex<Vec<Vec<DocId>>>>,
    copies: Arc<Mutex<Vec<Vec<DocId>>>>,
    allow_copy: bool,
    fail_at: Option<DocId>,
}

impl Probe {
    fn new(source: &dyn DocumentStore) -> Self {
        Self {
            source: source.snapshot().unwrap(),
            projections: Arc::default(),
            copies: Arc::default(),
            allow_copy: false,
            fail_at: None,
        }
    }
}

impl DocumentStore for Probe {
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
        assert!(
            self.allow_copy,
            "private projections must not decode opaque payloads"
        );
        self.source.get_stored(id)
    }
    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        assert!(
            self.allow_copy,
            "capture must not copy immutable private rows"
        );
        self.copies.lock().push(ids.to_vec());
        self.source.get_stored_many(ids)
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::RetainedDocumentPage> {
        assert!(
            self.allow_copy,
            "capture must not copy immutable private rows"
        );
        self.copies.lock().push(ids.to_vec());
        self.source.get_stored_many_controlled(ids, control)
    }
    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.source.get_metadata(id)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        if self.fail_at == Some(id) {
            return Err(uqa_core::memory::MemoryError::SizeOverflow.into());
        }
        self.source.contains_doc_id(id)
    }
    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        assert_ne!(field, "opaque");
        self.source.get_field(id, field)
    }
    fn get_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        assert!(!fields.contains(&"opaque"));
        self.projections.lock().push(ids.to_vec());
        self.source.get_fields_multi(ids, fields)
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        assert!(!fields.contains(&"opaque"));
        self.projections.lock().push(ids.to_vec());
        self.source
            .for_each_fields_multi_ref_with_presence(ids, fields, visitor)
    }
    fn get_shared_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<uqa_storage::SharedDocumentRow>>>> {
        self.source.get_shared_fields(ids, fields)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("private selections must not enumerate the entire source")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("private selection count must not inspect the source")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}

fn document(key: i64) -> StoredDocument {
    StoredDocument::with_metadata(
        [
            ("key".into(), Value::Int(key)),
            ("opaque".into(), Value::Str("payload".repeat(1024))),
        ]
        .into(),
        DocumentMetadata::with_tuple_xmin(41),
    )
}

fn source() -> MemoryDocumentStore {
    let mut rows = MemoryDocumentStore::new();
    for (id, key) in [(1, 10), (4, 40), (u64::MAX, 90)] {
        rows.put_stored(id, document(key)).unwrap();
    }
    rows
}

fn retained(probe: &Probe) -> DocumentChanges {
    DocumentChanges::default()
        .with_retained(
            probe.snapshot().unwrap(),
            selection([
                (1, true),
                (4, true),
                (8, true),
                (9, false),
                (u64::MAX, true),
            ]),
            &control(),
        )
        .unwrap()
}

#[test]
fn private_selection_retains_sources_and_normalizes_missing_rows_without_copying() {
    let mut rows = source();
    let probe = Probe::new(&rows);
    let changes = retained(&probe);
    assert!(probe.copies.lock().is_empty());
    assert!(probe.projections.lock().is_empty());
    assert_eq!(
        changes.changes().collect::<Vec<_>>(),
        vec![
            (1, true),
            (4, true),
            (8, false),
            (9, false),
            (u64::MAX, true)
        ]
    );
    assert_eq!(changes.len().unwrap(), 3);
    assert_eq!(changes.doc_ids().unwrap(), vec![1, 4, u64::MAX]);
    assert_eq!(changes.next_doc_ids(Some(1), 1).unwrap(), vec![4]);
    assert!(changes.next_doc_ids(Some(u64::MAX), 1).unwrap().is_empty());
    assert_eq!(changes.change_presence(8), Some(false));
    assert_eq!(changes.change_presence(7), None);
    assert_eq!(
        changes.get_metadata(1).unwrap().unwrap().tuple_xmin(),
        Some(41)
    );
    rows.clear().unwrap();
    let nested = changes.snapshot().unwrap().snapshot().unwrap();
    drop(changes);
    drop(rows);
    assert_eq!(nested.get_field(4, "key").unwrap(), Some(Value::Int(40)));
    assert_eq!(
        nested.get_fields_multi(&[4, 8, 1], &["key"]).unwrap(),
        [(1, vec![Value::Int(10)]), (4, vec![Value::Int(40)])].into()
    );
    assert!(probe.copies.lock().is_empty());
}

#[test]
fn shared_command_changes_preserve_old_views_and_borrow_payloads() {
    let probe = Probe::new(&source());
    let fields = Arc::new(document(70).into_fields());
    let mut changes = retained(&probe);
    changes
        .insert_shared(
            7,
            Some((Arc::clone(&fields), DocumentMetadata::with_tuple_xmin(52))),
            &control(),
        )
        .unwrap();
    let original = changes.clone();
    changes.insert_shared(1, None, &control()).unwrap();
    changes
        .insert_shared(
            7,
            Some((
                Arc::new(document(71).into_fields()),
                DocumentMetadata::with_tuple_xmin(53),
            )),
            &control(),
        )
        .unwrap();
    changes
        .extend(
            DocumentChanges::from_rows(
                BTreeMap::from([(4, None), (6, Some(document(60)))]),
                &control(),
            )
            .unwrap(),
            &control(),
        )
        .unwrap();
    original
        .for_each_fields_multi_ref(&[7], &["opaque"], &mut |_, values| {
            assert!(std::ptr::eq(values[0], &raw const fields["opaque"]));
            true
        })
        .unwrap();
    assert_eq!(original.get_field(1, "key").unwrap(), Some(Value::Int(10)));
    assert_eq!(original.get_field(7, "key").unwrap(), Some(Value::Int(70)));
    assert_eq!(
        original.get_metadata(7).unwrap().unwrap().tuple_xmin(),
        Some(52)
    );
    assert_eq!(changes.doc_ids().unwrap(), vec![6, 7, u64::MAX]);
    assert_eq!(changes.get_field(7, "key").unwrap(), Some(Value::Int(71)));
    assert!(probe.copies.lock().is_empty());
}

#[test]
fn capture_failure_and_cancellation_leave_retained_views_intact() {
    let mut probe = Probe::new(&source());
    probe.fail_at = Some(4);
    let original =
        DocumentChanges::from_rows(BTreeMap::from([(2, Some(document(20)))]), &control()).unwrap();
    let error = original
        .clone()
        .with_retained(
            probe.snapshot().unwrap(),
            selection([(1, true), (4, true)]),
            &control(),
        )
        .err()
        .unwrap();
    assert_eq!(
        crate::storage_errors::storage_error("capture", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(original.doc_ids().unwrap(), vec![2]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = original
        .clone()
        .with_retained(
            probe.snapshot().unwrap(),
            selection([(1, true)]),
            &uqa_storage::read_control::StorageReadControl::new(control().memory(), &cancellation),
        )
        .err()
        .unwrap();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert!(DocumentChanges::capture_owned(
        &probe,
        selection([(1, true)]),
        &uqa_storage::read_control::StorageReadControl::new(control().memory(), &cancellation)
    )
    .is_err());
    assert_eq!(original.get_field(2, "key").unwrap(), Some(Value::Int(20)));
}

#[test]
fn serialized_capture_copies_only_selected_rows_in_bounded_pages() {
    let mut rows = source();
    let mut probe = Probe::new(&rows);
    probe.allow_copy = true;
    let count = u64::try_from(crate::DEFAULT_BATCH_SIZE * 2 + 5).unwrap();
    let desired = selection((0..count).map(|id| (id, id % 2 == 0)));
    let changes = DocumentChanges::capture_owned(&probe, desired, &control()).unwrap();
    let copied = probe.copies.lock();
    assert_eq!(copied.len(), 3);
    assert!(copied
        .iter()
        .all(|page| page.len() <= crate::DEFAULT_BATCH_SIZE));
    assert!(copied.iter().flatten().all(|id| id % 2 == 0));
    rows.clear().unwrap();
    assert_eq!(changes.doc_ids().unwrap(), vec![4]);
    let captured = changes
        .into_rows()
        .collect::<StorageBackendResult<BTreeMap<_, _>>>()
        .unwrap();
    assert_eq!(captured.len(), usize::try_from(count).unwrap());
    assert_eq!(captured[&4], Some(document(40)));
    assert_eq!(captured[&1], None);
}

#[test]
fn changes_reject_mutation_without_reading_payloads() {
    let mut changes = retained(&Probe::new(&source()));
    assert!(changes.put(1, BTreeMap::new()).is_err());
    assert!(changes.put_stored(1, document(1)).is_err());
    assert!(changes.patch_fields(1, &BTreeMap::new()).is_err());
    assert!(changes.delete(1).is_err());
    assert!(changes.clear().is_err());
    assert!(changes.writable_snapshot().is_err());
    assert_eq!(changes.len().unwrap(), 3);
}

fn control() -> uqa_storage::read_control::StorageReadControl {
    uqa_storage::read_control::StorageReadControl::with_limit(1 << 20)
}

fn selection(
    rows: impl IntoIterator<Item = (DocId, bool)>,
) -> crate::query::document_changes::DocumentSelection {
    let control = control();
    let mut selected = crate::query::document_changes::DocumentSelection::new(&control);
    for (id, present) in rows {
        selected.insert(id, present, &control).unwrap();
    }
    selected
}
