//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_storage::{DocumentMetadata, StorageBackendResult};

fn columns(sql: &str) -> Vec<ColumnDef> {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(sql).unwrap().remove(0) else {
        unreachable!()
    };
    table
        .columns
        .into_iter()
        .enumerate()
        .map(|(i, mut column)| {
            column.object_id = Some([u8::try_from(i + 1).unwrap(); 16]);
            column
        })
        .collect()
}

fn document(values: &[(&str, Value)], xmin: u32) -> StoredDocument {
    StoredDocument::with_metadata(
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
        DocumentMetadata::with_tuple_xmin(xmin),
    )
}

fn schema<'a>(columns: &'a [ColumnDef], index: &'a dyn InvertedIndex) -> SnapshotSchema<'a> {
    SnapshotSchema {
        columns,
        analyzer: index.analyzer(),
        text_fields: &[],
        text_revisions: index,
        vector_dimensions: BTreeMap::new(),
    }
}

#[test]
fn base_column_incarnations_and_private_column_layouts_remain_distinct() {
    let source_columns = columns("CREATE TABLE t (a INTEGER, b INTEGER, gone INTEGER)");
    let mut target = vec![source_columns[0].clone(), source_columns[1].clone()];
    target[0].name = "b".into();
    target[1].name = "a".into();
    let mut reused = source_columns[2].clone();
    reused.object_id = Some([9; 16]);
    reused.missing_value = Some(Value::Int(17));
    target.push(reused);
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            document(
                &[
                    ("a", Value::Int(10)),
                    ("b", Value::Int(20)),
                    ("gone", Value::Int(30)),
                ],
                41,
            ),
        )
        .unwrap();
    source
        .put_stored(
            2,
            document(
                &[
                    ("a", Value::Int(11)),
                    ("b", Value::Int(21)),
                    ("gone", Value::Int(31)),
                ],
                42,
            ),
        )
        .unwrap();
    let private = document(
        &[
            ("a", Value::Int(101)),
            ("b", Value::Int(201)),
            ("gone", Value::Int(301)),
        ],
        51,
    );
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = materialize(
        &source,
        &source_columns,
        &schema(&target, &index),
        [(2, Some(private.clone()))].into(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(view.documents.get_stored(2).unwrap(), Some(private));
    assert_eq!(
        view.documents.get_stored(1).unwrap(),
        Some(document(
            &[
                ("a", Value::Int(20)),
                ("b", Value::Int(10)),
                ("gone", Value::Int(17))
            ],
            41
        ))
    );
    assert_eq!(source.get_field(1, "a").unwrap(), Some(Value::Int(10)));
}

struct PagedSource {
    rows: MemoryDocumentStore,
    requested: Mutex<Vec<DocId>>,
}

impl DocumentStore for PagedSource {
    fn put_stored(&mut self, id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.rows.put_stored(id, document)
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.rows.get_stored(id)
    }
    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        assert!(ids.len() <= crate::DEFAULT_BATCH_SIZE);
        self.requested.lock().extend_from_slice(ids);
        self.rows.get_stored_many(ids)
    }
    fn delete(&mut self, id: DocId) -> StorageBackendResult<()> {
        self.rows.delete(id)
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.rows.clear()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("snapshot reconstruction must page ids")
    }
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        assert!(limit <= crate::DEFAULT_BATCH_SIZE);
        self.rows.next_doc_ids(after, limit)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.rows.snapshot()
    }
}

#[test]
fn reconstruction_pages_rows_and_skips_private_replacements_and_deletions() {
    let mut source = PagedSource {
        rows: MemoryDocumentStore::new(),
        requested: Mutex::new(Vec::new()),
    };
    let count = u64::try_from(crate::DEFAULT_BATCH_SIZE * 2 + 7).unwrap();
    for id in 0..count {
        source
            .put_stored(
                id,
                document(&[("id", Value::Int(i64::try_from(id).unwrap()))], 19),
            )
            .unwrap();
    }
    let fields = columns("CREATE TABLE t (id INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let changes = [
        (0, None),
        (1, Some(document(&[("id", Value::Int(-1))], 21))),
        (count, Some(document(&[("id", Value::Int(-2))], 21))),
    ]
    .into();
    let view = materialize(
        &source,
        &fields,
        &schema(&fields, &index),
        changes,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(view.document_count, count);
    assert!(!source.requested.lock().contains(&0));
    assert!(!source.requested.lock().contains(&1));
    assert_eq!(
        source.requested.lock().len(),
        usize::try_from(count - 2).unwrap()
    );
    assert_eq!(view.documents.get(0).unwrap(), None);
    assert_eq!(
        view.documents.get_field(1, "id").unwrap(),
        Some(Value::Int(-1))
    );
    assert_eq!(
        view.documents.get_field(count, "id").unwrap(),
        Some(Value::Int(-2))
    );
}

#[test]
fn adapted_text_vectors_defaults_and_generated_fields_agree_with_rows() {
    let source_columns = columns("CREATE TABLE t (old TEXT, v VECTOR(2), id INTEGER)");
    let mut target = source_columns.clone();
    target[0].name = "body".into();
    let generated =
        columns("CREATE TABLE x (computed INTEGER GENERATED ALWAYS AS (id + 1) STORED)").remove(0);
    let mut generated = generated;
    generated.object_id = Some([9; 16]);
    target.push(generated);
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            document(
                &[
                    ("old", Value::Str("old word".into())),
                    ("v", Value::List(vec![Value::Float(1.0), Value::Float(0.0)])),
                    ("id", Value::Int(7)),
                ],
                31,
            ),
        )
        .unwrap();
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .set_field_analyzer(
            "body",
            uqa_analysis::keyword_analyzer(),
            uqa_storage::AnalyzerPhase::Search,
        )
        .unwrap();
    let fields = vec!["body".to_string()];
    let mut selected = schema(&target, &index);
    selected.text_fields = &fields;
    selected.vector_dimensions.insert("v".into(), 2);
    let view = materialize(
        &source,
        &source_columns,
        &selected,
        BTreeMap::new(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        view.documents.get_field(1, "computed").unwrap(),
        Some(Value::Int(8))
    );
    assert_eq!(
        view.text
            .get_posting_list("body", "word")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(Arc::ptr_eq(
        &view.text.search_analyzer_revision("body").unwrap(),
        &index.search_analyzer_revision("body").unwrap()
    ));
    assert_eq!(
        view.vectors["v"]
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn cancellation_prevents_any_source_read_or_partial_result() {
    let source = PagedSource {
        rows: MemoryDocumentStore::new(),
        requested: Mutex::new(Vec::new()),
    };
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(materialize(&source, &[], &schema(&[], &index), BTreeMap::new(), &cancel).is_err());
    assert!(source.requested.lock().is_empty());
}
