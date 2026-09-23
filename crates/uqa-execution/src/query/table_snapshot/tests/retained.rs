//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod generated;
mod lookup;
mod presence;
mod private;

#[derive(Clone)]
struct ProjectedSource {
    rows: Arc<dyn DocumentStore>,
    pages: Arc<AtomicUsize>,
    projections: Arc<AtomicUsize>,
    borrowing: Arc<AtomicBool>,
    cancel_after_page: Option<CancellationToken>,
    cancel_after_projection: Option<(CancellationToken, Arc<AtomicBool>)>,
    forbid_owned_fields: bool,
}

impl ProjectedSource {
    fn new(rows: &dyn DocumentStore) -> Self {
        Self {
            rows: rows.snapshot().unwrap(),
            pages: Arc::default(),
            projections: Arc::default(),
            borrowing: Arc::default(),
            cancel_after_page: None,
            cancel_after_projection: None,
            forbid_owned_fields: false,
        }
    }
}

impl DocumentStore for ProjectedSource {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("retained projections must not decode opaque row payloads")
    }
    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.rows.get_metadata(id)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        assert!(!self.borrowing.load(Ordering::Relaxed));
        self.rows.contains_doc_id(id)
    }
    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        assert!(
            !self.forbid_owned_fields,
            "projection must borrow its values"
        );
        assert!(!self.borrowing.load(Ordering::Relaxed));
        assert_ne!(field, "opaque");
        self.projections.fetch_add(1, Ordering::Relaxed);
        self.rows.get_field(id, field)
    }
    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<bool>> {
        assert!(
            !self.borrowing.load(Ordering::Relaxed),
            "metadata must precede the provider borrow"
        );
        self.rows.field_presence_controlled(ids, fields, control)
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        assert!(!fields.contains(&"opaque"));
        self.projections.fetch_add(1, Ordering::Relaxed);
        assert!(!self.borrowing.swap(true, Ordering::Relaxed));
        let result = self
            .rows
            .for_each_fields_multi_ref_with_presence(ids, fields, visitor);
        self.borrowing.store(false, Ordering::Relaxed);
        if let Some((token, armed)) = &self.cancel_after_projection {
            if armed.swap(false, Ordering::Relaxed) {
                token.cancel();
            }
        }
        result
    }
    fn get_shared_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<uqa_storage::SharedDocumentRow>>>> {
        assert!(!fields.contains(&"opaque"));
        self.rows.get_shared_fields(ids, fields)
    }
    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::SharedDocumentRow)>>> {
        self.rows.next_shared_fields(after, limit, fields)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("retained views must page source identities")
    }
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        assert!(limit <= crate::DEFAULT_BATCH_SIZE);
        self.pages.fetch_add(1, Ordering::Relaxed);
        let ids = self.rows.next_doc_ids(after, limit)?;
        if let Some(token) = &self.cancel_after_page {
            token.cancel();
        }
        Ok(ids)
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        assert!(limit <= crate::DEFAULT_BATCH_SIZE);
        self.pages.fetch_add(1, Ordering::Relaxed);
        let ids = self.rows.next_doc_ids_controlled(after, limit, control)?;
        if let Some(token) = &self.cancel_after_page {
            token.cancel();
        }
        Ok(ids)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}

fn renamed_documents() -> (Vec<ColumnDef>, Vec<ColumnDef>, MemoryDocumentStore) {
    let columns = columns("CREATE TABLE t (a INTEGER, opaque TEXT)");
    let mut target = columns.clone();
    target[0].name = "renamed".into();
    let mut added = target[0].clone();
    added.name = "added".into();
    added.object_id = Some([9; 16]);
    added.missing_value = Some(Value::Int(17));
    target.push(added);
    let mut rows = MemoryDocumentStore::new();
    for (id, value) in [(1, 10), (3, 30), (5, 50), (u64::MAX, 90)] {
        rows.put_stored(
            id,
            document(
                &[
                    ("a", Value::Int(value)),
                    ("opaque", Value::Str("payload".repeat(2048))),
                ],
                41,
            ),
        )
        .unwrap();
    }
    (columns, target, rows)
}

#[test]
fn retained_private_views_capture_no_base_rows_and_project_in_requested_order() {
    let (columns, target, mut rows) = renamed_documents();
    let source = ProjectedSource::new(&rows);
    let pages = Arc::clone(&source.pages);
    let projections = Arc::clone(&source.projections);
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let changes: BTreeMap<_, _> = [
        (2, Some(document(&[("renamed", Value::Int(200))], 51))),
        (
            3,
            Some(document(
                &[("renamed", Value::Int(300)), ("added", Value::Int(99))],
                52,
            )),
        ),
        (5, None),
    ]
    .into();
    let mut view = retain(
        Arc::new(source),
        &columns,
        &schema(&target, &index),
        DocumentChanges::from_rows(changes, &control()).unwrap(),
        &control(),
    )
    .unwrap();
    assert_eq!(pages.load(Ordering::Relaxed), 0);
    assert_eq!(projections.load(Ordering::Relaxed), 0);
    assert_eq!(view.document_count, 4);
    assert_eq!(view.text.doc_count().unwrap(), 0);
    assert!(view.documents.put(99, BTreeMap::new()).is_err());
    assert!(view.documents.patch_fields(99, &BTreeMap::new()).is_err());
    assert!(view.documents.delete(99).is_err());
    assert!(view.documents.clear().is_err());
    rows.clear().unwrap();
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    drop(view);
    let projected = nested
        .get_fields_multi(&[3, 1, 99, 2], &["renamed", "added"])
        .unwrap();
    assert_eq!(
        projected,
        [
            (1, vec![Value::Int(10), Value::Int(17)]),
            (2, vec![Value::Int(200), Value::Int(17)]),
            (3, vec![Value::Int(300), Value::Int(99)]),
        ]
        .into()
    );
    let mut visited = Vec::new();
    nested
        .for_each_fields_multi_ref_with_presence(
            &[3, 1, 99, 2, 3],
            &["renamed"],
            &mut |id, present, values| {
                visited.push((id, present, values[0].clone()));
                visited.len() < 3
            },
        )
        .unwrap();
    assert_eq!(
        visited,
        vec![
            (3, true, Value::Int(300)),
            (1, true, Value::Int(10)),
            (99, false, Value::Null)
        ]
    );
    assert_eq!(
        nested.get_field(1, "renamed").unwrap(),
        Some(Value::Int(10))
    );
    assert_eq!(nested.get_field(1, "a").unwrap(), None);
    assert_eq!(
        nested.get_metadata(3).unwrap().unwrap().tuple_xmin(),
        Some(52)
    );
    assert_eq!(
        nested.get_metadata(1).unwrap().unwrap().tuple_xmin(),
        Some(41)
    );
    assert_eq!(nested.doc_ids().unwrap(), vec![1, 2, 3, u64::MAX]);
    assert_eq!(nested.next_doc_ids(Some(1), 2).unwrap(), vec![2, 3]);
    assert!(nested.next_doc_ids(Some(u64::MAX), 2).unwrap().is_empty());
    assert!(nested.next_doc_ids(None, 0).unwrap().is_empty());
    assert!(!nested.contains_doc_id(5).unwrap());
    assert!(nested.writable_snapshot().is_err());
}

fn defaulted_documents() -> (Vec<ColumnDef>, Vec<ColumnDef>, MemoryDocumentStore) {
    let mut columns = columns("CREATE TABLE t (a INTEGER, b INTEGER)");
    columns[0].missing_value = Some(Value::Int(17));
    let mut target = columns.clone();
    target[1].name = "renamed".into();
    let mut added = columns[0].clone();
    added.name = "c".into();
    added.object_id = Some([8; 16]);
    target.push(added);
    let mut generated =
        super::columns("CREATE TABLE t (g INTEGER GENERATED ALWAYS AS (a + renamed) VIRTUAL)")
            .remove(0);
    generated.object_id = Some([9; 16]);
    target.push(generated);
    let mut source = MemoryDocumentStore::new();
    for (id, fields) in [
        (1, vec![("b", Value::Int(2))]),
        (2, vec![("a", Value::Null), ("b", Value::Int(3))]),
        (
            4,
            vec![
                ("a", Value::Int(1)),
                ("renamed", Value::Int(99)),
                ("c", Value::Int(23)),
            ],
        ),
        (
            5,
            vec![
                ("a", Value::Int(2)),
                ("b", Value::Null),
                ("renamed", Value::Int(99)),
                ("c", Value::Null),
            ],
        ),
        (
            6,
            vec![
                ("a", Value::Int(3)),
                ("b", Value::Int(5)),
                ("renamed", Value::Int(99)),
            ],
        ),
    ] {
        source
            .put_stored(id, document(&fields, 40 + u32::try_from(id).unwrap()))
            .unwrap();
    }
    (columns, target, source)
}

#[test]
fn retained_projection_preserves_absent_defaults_explicit_nulls_and_generated_values() {
    let (columns, target, source) = defaulted_documents();
    let changes: BTreeMap<_, _> = [(
        3,
        Some(document(
            &[("a", Value::Int(8)), ("renamed", Value::Int(1))],
            51,
        )),
    )]
    .into();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let selected = schema(&target, &index);
    let expected = materialize(
        &source,
        &columns,
        &selected,
        DocumentChanges::from_rows(BTreeMap::clone(&changes), &control()).unwrap(),
        &control(),
    )
    .unwrap();
    let actual = retain(
        source.snapshot().unwrap(),
        &columns,
        &selected,
        DocumentChanges::from_rows(changes, &control()).unwrap(),
        &control(),
    )
    .unwrap();
    for id in [1, 2, 3, 4, 5, 6, 9] {
        assert_eq!(
            actual.documents.get_stored(id).unwrap(),
            expected.documents.get_stored(id).unwrap()
        );
        for field in ["a", "b", "renamed", "c", "g", "missing"] {
            assert_eq!(
                actual.documents.get_field(id, field).unwrap(),
                expected.documents.get_field(id, field).unwrap(),
                "{id}: {field}"
            );
        }
    }
    assert_eq!(
        actual
            .documents
            .get_fields_multi(&[1, 2, 3, 4, 5, 6, 9], &["g", "a", "renamed", "c"])
            .unwrap(),
        expected
            .documents
            .get_fields_multi(&[1, 2, 3, 4, 5, 6, 9], &["g", "a", "renamed", "c"])
            .unwrap()
    );
    assert_eq!(
        actual.documents.get_field(1, "g").unwrap(),
        Some(Value::Int(19))
    );
    assert_eq!(
        actual.documents.get_field(2, "g").unwrap(),
        Some(Value::Null)
    );
    assert_eq!(
        actual.documents.get_field(3, "g").unwrap(),
        Some(Value::Int(9))
    );
    let projected = retain(
        Arc::new(ProjectedSource::new(&source)),
        &columns,
        &selected,
        DocumentChanges::default(),
        &control(),
    )
    .unwrap();
    assert_eq!(
        projected
            .documents
            .get_fields_multi(&[1, 2, 4, 5, 6], &["a", "renamed", "c"])
            .unwrap(),
        expected
            .documents
            .get_fields_multi(&[1, 2, 4, 5, 6], &["a", "renamed", "c"])
            .unwrap()
    );
}

#[test]
fn retained_renamed_projection_resolves_missing_fields_after_releasing_source() {
    let (columns, target, source) = defaulted_documents();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(ProjectedSource::new(&source)),
        &columns,
        &schema(&target, &index),
        DocumentChanges::default(),
        &control(),
    )
    .unwrap();
    assert!(view
        .documents
        .get_shared_fields(&[4, 5, 6], &["renamed"])
        .unwrap()
        .is_none());
    assert!(view
        .documents
        .next_shared_fields(None, 10, &["renamed"])
        .unwrap()
        .is_none());
    assert_eq!(
        view.documents
            .get_fields_multi(&[4, 5, 6], &["renamed"])
            .unwrap(),
        [
            (4, vec![Value::Int(99)]),
            (5, vec![Value::Null]),
            (6, vec![Value::Int(5)])
        ]
        .into()
    );
    let mut visited = Vec::new();
    view.documents
        .for_each_fields_multi_ref_with_presence(
            &[4, 6, 5, 4, 9],
            &["renamed"],
            &mut |id, present, values| {
                visited.push((id, present, values[0].clone()));
                visited.len() < 3
            },
        )
        .unwrap();
    assert_eq!(
        visited,
        [
            (4, true, Value::Int(99)),
            (6, true, Value::Int(5)),
            (5, true, Value::Null)
        ]
    );
}

#[test]
fn retained_index_reconstruction_reads_only_indexed_fields() {
    let columns = columns("CREATE TABLE t (body TEXT, v VECTOR(2), opaque TEXT)");
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            document(
                &[
                    ("body", Value::Str("original".into())),
                    ("v", Value::List(vec![Value::Float(1.0), Value::Float(0.0)])),
                    ("opaque", Value::Str("payload".repeat(2048))),
                ],
                41,
            ),
        )
        .unwrap();
    let probe = ProjectedSource::new(&source);
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let fields = vec!["body".to_string()];
    let mut selected = schema(&columns, &index);
    selected.text_fields = &fields;
    selected.vector_dimensions.insert("v".into(), 2);
    let changes: BTreeMap<_, _> = [(
        2,
        Some(document(
            &[
                ("body", Value::Str("private".into())),
                ("v", Value::List(vec![Value::Float(0.0), Value::Float(1.0)])),
                ("opaque", Value::Str("private payload".repeat(2048))),
            ],
            51,
        )),
    )]
    .into();
    let view = retain(
        Arc::new(probe),
        &columns,
        &selected,
        DocumentChanges::from_rows(changes, &control()).unwrap(),
        &control(),
    )
    .unwrap();
    assert_eq!(view.document_count, 2);
    assert_eq!(view.text.doc_count().unwrap(), 2);
    assert_eq!(
        view.text
            .get_posting_list("body", "original")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        view.text
            .get_posting_list("body", "private")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(
        view.vectors["v"]
            .search_knn(&[0.0, 1.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn retained_renamed_rows_keep_shared_projections() {
    let columns = columns("CREATE TABLE t (a INTEGER, opaque TEXT)");
    let mut target = columns.clone();
    target[0].name = "renamed".into();
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            document(
                &[
                    ("a", Value::Int(11)),
                    ("opaque", Value::Str("hidden".repeat(2048))),
                ],
                41,
            ),
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(ProjectedSource::new(&source)),
        &columns,
        &schema(&target, &index),
        DocumentChanges::default(),
        &control(),
    )
    .unwrap();
    let shared = view
        .documents
        .get_shared_fields(&[1, 9], &["renamed"])
        .unwrap()
        .unwrap();
    assert!(shared[1].is_none());
    shared[0]
        .as_ref()
        .unwrap()
        .with_projected(|values| assert_eq!(values, &[&Value::Int(11)]));
    let page = view
        .documents
        .next_shared_fields(None, 10, &["renamed"])
        .unwrap()
        .unwrap();
    assert_eq!(page.len(), 1);
    let mut visits = Vec::new();
    assert_eq!(
        view.documents
            .for_each_next_fields(None, 10, &["renamed"], &mut |id, values| {
                visits.push((id, values[0].clone()));
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(visits, vec![(1, Value::Int(11))]);
}

#[test]
fn retained_identity_pages_cross_deleted_ranges_without_reading_rows() {
    let mut source = MemoryDocumentStore::new();
    let count = u64::try_from(crate::DEFAULT_BATCH_SIZE * 2 + 7).unwrap();
    for id in 0..count {
        source
            .put_stored(id, document(&[("a", Value::Int(1))], 41))
            .unwrap();
    }
    let mut changes = (0..count - 2)
        .map(|id| (id, None))
        .collect::<BTreeMap<_, _>>();
    changes.insert(count, Some(document(&[("a", Value::Int(2))], 51)));
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(ProjectedSource::new(&source)),
        &[],
        &schema(&[], &index),
        DocumentChanges::from_rows(changes, &control()).unwrap(),
        &control(),
    )
    .unwrap();
    assert_eq!(view.document_count, 3);
    assert_eq!(
        view.documents.next_doc_ids(None, 2).unwrap(),
        vec![count - 2, count - 1]
    );
    assert_eq!(
        view.documents.next_doc_ids(Some(count - 1), 1).unwrap(),
        vec![count]
    );
}
