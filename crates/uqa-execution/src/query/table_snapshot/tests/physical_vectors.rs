//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::document_changes::DocumentSelection;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_storage::diskann_index::{
    build::DiskANNTemporaryBudget, DiskANNIndexOptions, DiskANNMemoryIndex,
};
use uqa_storage::vector_index::DiskANNIndexParams;

#[derive(Clone)]
struct ProjectionProbe {
    source: Arc<dyn DocumentStore>,
    projections: Arc<AtomicUsize>,
}

impl DocumentStore for ProjectionProbe {
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
        self.source.get_stored(id)
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::RetainedDocumentPage> {
        self.source.get_stored_many_controlled(ids, control)
    }
    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.source.contains_doc_id(id)
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.source.doc_ids()
    }
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        self.source.next_doc_ids(after, limit)
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        self.source.next_doc_ids_controlled(after, limit, control)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.source.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.projections.fetch_add(1, Ordering::Relaxed);
        self.source
            .for_each_fields_multi_ref_with_presence(ids, fields, visitor)
    }
}

fn vector(x: f64, y: f64) -> Value {
    Value::List(vec![Value::Float(x), Value::Float(y)])
}

fn indexes(control: &StorageReadControl) -> BTreeMap<FieldName, Box<dyn VectorIndex>> {
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 2,
        beam_width: 1,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).unwrap()
    };
    let mut options = DiskANNIndexOptions::for_parameters(parameters);
    options.merge.sort_buffer_records = 8;
    options.generation.code_batch_nodes = 4;
    options.generation.side_batch_entries = 4;
    let mut index =
        DiskANNMemoryIndex::new(2, options, &DiskANNTemporaryBudget::new(1 << 20), control)
            .unwrap();
    for (id, value) in [
        (1, vec![1.0, 0.0]),
        (2, vec![0.0, 1.0]),
        (3, vec![-1.0, 0.0]),
    ] {
        index.add(id, value).unwrap();
    }
    index.initialize().unwrap();
    BTreeMap::from([("v".into(), Box::new(index) as Box<dyn VectorIndex>)])
}

fn desired(rows: &[(DocId, bool)], control: &StorageReadControl) -> DocumentSelection {
    let mut selected = DocumentSelection::new(control);
    for (id, present) in rows {
        selected.insert(*id, *present, control).unwrap();
    }
    selected
}

fn scores(index: &dyn VectorIndex) -> Vec<(DocId, f64)> {
    index
        .search_knn(&[1.0, 0.0], 99)
        .unwrap()
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score))
        .collect()
}

#[test]
fn diskann_private_table_copies_retain_evaluated_sources_without_vector_projection() {
    let control = StorageReadControl::with_limit(64 << 20);
    let columns = columns("CREATE TABLE t (v VECTOR(2))");
    let mut live = indexes(&control);
    let base = VectorIndexes::capture(&live, &control).unwrap();
    let mut rows = MemoryDocumentStore::new();
    for (id, value) in [
        (1, vector(1.0, 0.0)),
        (2, vector(0.0, 1.0)),
        (3, vector(-1.0, 0.0)),
    ] {
        rows.put_stored(id, document(&[("v", value)], 41)).unwrap();
    }
    let probe = ProjectionProbe {
        source: rows.snapshot().unwrap(),
        projections: Arc::default(),
    };
    rows.put_stored(1, document(&[("v", vector(-1.0, 0.0))], 42))
        .unwrap();
    rows.put_stored(2, document(&[("v", vector(1.0, 0.0))], 42))
        .unwrap();
    rows.put_stored(4, document(&[("v", vector(1.0, 0.0))], 42))
        .unwrap();
    let index = live.get_mut("v").unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    index.add(2, vec![1.0, 0.0]).unwrap();
    index.add(4, vec![1.0, 0.0]).unwrap();
    // The evaluated deletion must hide row 3 even while its original source still retains it.
    let changes = DocumentChanges::default()
        .with_retained_vectors(
            rows.snapshot().unwrap(),
            desired(&[(1, true), (3, false), (4, true)], &control),
            &columns,
            &live,
            &control,
        )
        .unwrap();
    let captured = changes.clone();
    rows.put_stored(1, document(&[("v", vector(0.0, 1.0))], 43))
        .unwrap();
    live.get_mut("v").unwrap().add(1, vec![0.0, 1.0]).unwrap();
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut selected = schema(&columns, &text);
    selected.vector_dimensions = &live;
    let table = retain_with_vector_indexes(
        probe.snapshot().unwrap(),
        &columns,
        &selected,
        changes,
        Some(&base),
        &control,
    )
    .unwrap();
    assert_eq!(probe.projections.load(Ordering::Relaxed), 0);
    let index = table.vectors.get("v").unwrap();
    assert_eq!(index.index_kind(), "diskann");
    assert_eq!(scores(index), [(1, -1.0), (2, 0.0), (4, 1.0)]);
    assert_eq!(index.count().unwrap(), 3);
    assert_eq!(
        table.documents.get_field(1, "v").unwrap(),
        Some(vector(-1.0, 0.0))
    );
    assert_eq!(
        table.documents.get_field(2, "v").unwrap(),
        Some(vector(0.0, 1.0))
    );
    assert!(!table.documents.contains_doc_id(3).unwrap());
    assert_eq!(
        captured
            .get_stored(1)
            .unwrap()
            .unwrap()
            .metadata()
            .tuple_xmin(),
        Some(42)
    );
    let page = captured
        .get_stored_many_controlled(&[1, 3, 4], &control)
        .unwrap();
    assert!(page[0].is_some());
    assert!(page[1].is_none());
    assert!(page[2].is_some());
    drop(page);
    let nested = VectorIndexes::capture(&table.vectors, &control).unwrap();
    drop((table, base, live, rows, probe, captured));
    assert_eq!(
        scores(nested.get("v").unwrap()),
        [(1, -1.0), (2, 0.0), (4, 1.0)]
    );
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_private_sources_follow_column_incarnations_and_keep_each_capture() {
    let control = StorageReadControl::with_limit(64 << 20);
    let mut columns = columns("CREATE TABLE t (v VECTOR(2))");
    let mut live = indexes(&control);
    let base = VectorIndexes::capture(&live, &control).unwrap();
    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(1, document(&[("v", vector(-1.0, 0.0))], 42))
        .unwrap();
    rows.put_stored(2, document(&[("v", vector(1.0, 0.0))], 42))
        .unwrap();
    live.get_mut("v").unwrap().add(1, vec![-1.0, 0.0]).unwrap();
    let changes = DocumentChanges::default()
        .with_retained_vectors(
            rows.snapshot().unwrap(),
            desired(&[(1, true)], &control),
            &columns,
            &live,
            &control,
        )
        .unwrap();
    live.get_mut("v").unwrap().add(1, vec![0.0, 1.0]).unwrap();
    live.get_mut("v").unwrap().add(2, vec![1.0, 0.0]).unwrap();
    let changes = changes
        .with_retained_vectors(
            rows.snapshot().unwrap(),
            desired(&[(2, true)], &control),
            &columns,
            &live,
            &control,
        )
        .unwrap();
    columns[0].name = "renamed".into();
    let selected = changes
        .diskann_read_changes("renamed", Some(&columns[0]), &control)
        .unwrap()
        .unwrap();
    let index = base
        .get("v")
        .unwrap()
        .snapshot_with_diskann_changes(&selected, &control)
        .unwrap()
        .unwrap();
    assert_eq!(scores(&*index), [(1, -1.0), (2, 1.0), (3, -1.0)]);
    columns[0].object_id = Some([9; 16]);
    assert!(changes
        .diskann_read_changes("v", Some(&columns[0]), &control)
        .unwrap()
        .is_none());
    let used = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    assert!(changes.diskann_read_changes("v", None, &control).is_err());
    drop(occupied);
    assert_eq!(control.memory().used(), used);
    control.cancellation().cancel();
    assert!(changes.diskann_read_changes("v", None, &control).is_err());
    drop((index, selected, changes, base, live));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_complete_row_replacements_keep_the_selected_definition_without_projection() {
    let control = StorageReadControl::with_limit(64 << 20);
    let columns = columns("CREATE TABLE t (v VECTOR(2))");
    let base = indexes(&control);
    let mut rows = MemoryDocumentStore::new();
    for (id, value) in [
        (1, vector(1.0, 0.0)),
        (2, vector(0.0, 1.0)),
        (3, vector(-1.0, 0.0)),
    ] {
        rows.put_stored(id, document(&[("v", value)], 41)).unwrap();
    }
    let probe = ProjectionProbe {
        source: rows.snapshot().unwrap(),
        projections: Arc::default(),
    };
    let mut live = indexes(&control);
    let index = live.get_mut("v").unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    index.add(2, vec![1.0, 0.0]).unwrap();
    index.add(4, vec![1.0, 0.0]).unwrap();
    rows.put_stored(1, document(&[("v", vector(-1.0, 0.0))], 42))
        .unwrap();
    rows.put_stored(4, document(&[("v", vector(1.0, 0.0))], 42))
        .unwrap();
    let changes = DocumentChanges::default()
        .with_retained_vectors(
            rows.snapshot().unwrap(),
            desired(&[(1, true), (2, false), (3, false), (4, true)], &control),
            &columns,
            &live,
            &control,
        )
        .unwrap();
    let partial = DocumentChanges::default()
        .with_retained_vectors(
            rows.snapshot().unwrap(),
            desired(&[(1, true), (3, false), (4, true)], &control),
            &columns,
            &live,
            &control,
        )
        .unwrap();
    // The selected index also contains rows outside this fixed query and a later version of row 1.
    live.get_mut("v").unwrap().add(1, vec![0.0, 1.0]).unwrap();
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut selected = schema(&columns, &text);
    selected.vector_dimensions = &live;
    let table = retain_with_vector_indexes(
        probe.snapshot().unwrap(),
        &columns,
        &selected,
        changes,
        Some(&base),
        &control,
    )
    .unwrap();
    assert_eq!(probe.projections.load(Ordering::Relaxed), 0);
    let index = table.vectors.get("v").unwrap();
    assert_eq!(index.index_kind(), "diskann");
    assert_eq!(scores(index), [(1, -1.0), (4, 1.0)]);
    assert_eq!(table.document_count, 2);
    assert_eq!(
        table.documents.get_field(1, "v").unwrap(),
        Some(vector(-1.0, 0.0))
    );
    assert!(!table.documents.contains_doc_id(2).unwrap());
    assert!(!table.documents.contains_doc_id(3).unwrap());
    let nested = VectorIndexes::capture(&table.vectors, &control).unwrap();
    let partial = retain_with_vector_indexes(
        probe.snapshot().unwrap(),
        &columns,
        &selected,
        partial,
        Some(&base),
        &control,
    )
    .unwrap();
    assert_eq!(
        scores(partial.vectors.get("v").unwrap()),
        [(1, -1.0), (2, 0.0), (4, 1.0)]
    );
    assert_eq!(
        partial.documents.get_field(2, "v").unwrap(),
        Some(vector(0.0, 1.0))
    );
    drop(partial);
    drop((table, rows, base, live, probe));
    assert_eq!(scores(nested.get("v").unwrap()), [(1, -1.0), (4, 1.0)]);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}
