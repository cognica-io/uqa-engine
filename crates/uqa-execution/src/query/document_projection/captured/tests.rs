//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::memory::MemoryBudget;
use uqa_storage::{
    document_store::{Document, MemoryDocumentStore, StoredDocument},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

fn definitions(sql: &str) -> Vec<ColumnDef> {
    let uqa_sql::ast::Statement::CreateTable(table) = uqa_sql::compile(sql).unwrap().remove(0)
    else {
        panic!("table declaration");
    };
    table.columns
}

fn stored(value: i64, xmin: u32) -> StoredDocument {
    StoredDocument::with_metadata(
        Document::from([("value".into(), Value::Int(value))]),
        DocumentMetadata::with_tuple_xmin(xmin),
    )
}

#[test]
fn captured_field_projection_keeps_private_generated_values_and_tuple_metadata() {
    let mut source = MemoryDocumentStore::new();
    source.put_stored(1, stored(1, 4)).unwrap();
    source.put_stored(2, stored(2, 5)).unwrap();
    let cancellation = CancellationToken::new();
    let control = StorageReadControl::new(&MemoryBudget::new(1024 * 1024), &cancellation);
    let mut changes = DocumentChanges::default();
    for (id, value, xmin) in [(1, 5, 9), (3, 3, 10)] {
        let row = stored(value, xmin);
        changes
            .insert_shared(
                id,
                Some((Arc::new(row.fields().clone()), row.metadata())),
                &control,
            )
            .unwrap();
    }
    let columns = definitions(
        "CREATE TABLE t(value integer, derived integer GENERATED ALWAYS AS (value*2) VIRTUAL)",
    );
    let projection = FilterProjection {
        source: &source,
        changes: Some(&changes),
        definitions: &columns,
    };
    let ids = SelectedIds::capture([Ok(1), Ok(2), Ok(3)], 0, &cancellation).unwrap();
    for (field, value, expected) in [
        ("value", 2, vec![2]),
        ("derived", 10, vec![1]),
        ("xmin", 9, vec![1]),
    ] {
        let result = projection
            .capture(
                "t",
                &ids,
                field,
                &Predicate::Equals(Value::Int(value)),
                0,
                &cancellation,
            )
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                assert_eq!(entry.score.to_bits(), 0.0_f64.to_bits());
                entry.doc_id
            })
            .collect::<Vec<_>>();
        assert_eq!(result, expected, "{field}");
    }
}

struct ReadFailure {
    source: MemoryDocumentStore,
    fail: bool,
}
impl DocumentStore for ReadFailure {
    fn put_stored(&mut self, id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.source.put_stored(id, document)
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        if self.fail && id == 2 {
            return Err(StorageBackendError::Other("projection disk error".into()));
        }
        self.source.get_stored(id)
    }
    fn delete(&mut self, id: DocId) -> StorageBackendResult<()> {
        self.source.delete(id)
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.source.clear()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.source.doc_ids()
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.source.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.source.snapshot()
    }
}

#[test]
fn captured_field_projection_keeps_storage_and_private_phase_error_order() {
    let mut source = ReadFailure {
        source: MemoryDocumentStore::new(),
        fail: false,
    };
    let row = |value, text: &str| {
        Document::from([
            ("v".into(), Value::Int(value)),
            ("input".into(), Value::Str(text.into())),
        ])
    };
    source.put(1, row(0, "1")).unwrap();
    source.put(2, row(0, "2")).unwrap();
    source.fail = true;
    let columns = definitions("CREATE TABLE t(v integer, input text, derived integer GENERATED ALWAYS AS (CASE WHEN v=0 THEN 10/v ELSE input::integer END) VIRTUAL)");
    let cancellation = CancellationToken::new();
    let ids = SelectedIds::capture([Ok(1), Ok(2)], 0, &cancellation).unwrap();
    let execute = |source: &ReadFailure, changes: Option<&DocumentChanges>| {
        FilterProjection {
            source,
            changes,
            definitions: &columns,
        }
        .capture(
            "t",
            &ids,
            "derived",
            &Predicate::IsNotNull,
            0,
            &cancellation,
        )
        .err()
        .unwrap()
    };
    assert!(execute(&source, None)
        .to_string()
        .contains("projection disk error"));
    source.fail = false;
    assert_eq!(execute(&source, None).sqlstate(), Some("22012"));
    let control = StorageReadControl::new(&MemoryBudget::new(1024 * 1024), &cancellation);
    let mut changes = DocumentChanges::default();
    changes
        .insert_shared(
            1,
            Some((Arc::new(row(1, "invalid")), DocumentMetadata::default())),
            &control,
        )
        .unwrap();
    assert_eq!(execute(&source, Some(&changes)).sqlstate(), Some("22012"));
    source.put(2, row(1, "7")).unwrap();
    assert_eq!(execute(&source, Some(&changes)).sqlstate(), Some("22P02"));
}

#[test]
fn captured_field_projection_bounds_wide_values_and_releases_cancelled_input() {
    let mut source = MemoryDocumentStore::new();
    for id in 0..513 {
        source
            .put(
                id,
                Document::from([("body".into(), Value::Str("x".repeat(32 * 1024)))]),
            )
            .unwrap();
    }
    let columns = definitions("CREATE TABLE t(body text)");
    let cancellation = CancellationToken::new();
    let ids = SelectedIds::capture((0..513).map(Ok), 4096, &cancellation).unwrap();
    let projection = FilterProjection {
        source: &source,
        changes: None,
        definitions: &columns,
    };
    let allocated = allocation_counter::measure(|| {
        let output = projection
            .capture(
                "t",
                &ids,
                "body",
                &Predicate::IsNotNull,
                4096,
                &cancellation,
            )
            .unwrap();
        for (id, entry) in output.enumerate() {
            assert_eq!(entry.unwrap().doc_id, id as u64);
        }
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
    cancellation.cancel();
    assert_eq!(
        projection
            .capture(
                "t",
                &ids,
                "body",
                &Predicate::IsNotNull,
                4096,
                &cancellation
            )
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
}
