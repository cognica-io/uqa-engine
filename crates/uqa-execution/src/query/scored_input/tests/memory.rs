//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::scored_input::{ScoredDocumentSource, ScoredInput};
use crate::query::table_read::TableRead;
use crate::RowSource;
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{DocId, Value};
use uqa_storage::{DocumentStore, MemoryDocumentStore, StorageBackendResult, StoredDocument};

struct Documents {
    rows: MemoryDocumentStore,
    shared: bool,
    visited: Arc<AtomicUsize>,
}

impl DocumentStore for Documents {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("ordered projection must not point-read documents")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("ordered projection must not enumerate the table")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        unreachable!()
    }
    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::document_store::SharedDocumentRow)>>>
    {
        if self.shared {
            let result = self.rows.next_shared_fields(after, limit, fields)?;
            self.visited
                .fetch_add(result.as_ref().unwrap().len(), Ordering::Relaxed);
            Ok(result)
        } else {
            Ok(None)
        }
    }
    fn for_each_next_fields_borrowed(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        assert!(!self.shared);
        let result = self
            .rows
            .for_each_next_fields(after, limit, fields, visitor)?;
        self.visited.fetch_add(result.unwrap(), Ordering::Relaxed);
        Ok(result)
    }
}

struct Table(RwLock<Box<dyn DocumentStore>>);

impl TableRead for Table {
    fn column_definitions(&self) -> Vec<uqa_sql::ast::ColumnDef> {
        Vec::new()
    }
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.0.read()
    }
    fn maps_integer_keys(&self) -> bool {
        false
    }
}

#[test]
fn ordered_document_batches_bound_wide_projection_and_resume_exactly() {
    for (shared, local) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut documents = MemoryDocumentStore::new();
        for id in 1..=128 {
            documents
                .put(
                    id,
                    [
                        ("id".into(), Value::Int(id as i64)),
                        ("body".into(), Value::Str("한".repeat(8192))),
                    ]
                    .into(),
                )
                .unwrap();
        }
        let visited = Arc::new(AtomicUsize::new(0));
        let table = Arc::new(Table(RwLock::new(Box::new(Documents {
            rows: documents,
            shared,
            visited: Arc::clone(&visited),
        }))));
        let allocation = allocation_counter::measure(|| {
            let mut source = source(table, local);
            let mut expected = 1;
            loop {
                let rows = source.next_physical_batch(1024).unwrap();
                assert!(rows.len() <= 3, "unbounded projected batch");
                if rows.is_empty() {
                    break;
                }
                for row in rows {
                    assert_eq!(row.value(0), Some(&Value::Int(expected)));
                    assert_eq!(row.value(2), Some(&Value::Null));
                    assert!(
                        matches!(row.value(1), Some(Value::Str(text)) if text.len() == 24576 && text.chars().all(|c| c == '한'))
                    );
                    expected += 1;
                }
            }
            assert_eq!(expected, 129);
        });
        assert!(
            allocation.bytes_max < 512 * 1024,
            "shared={shared}, local={local}: {allocation:?}"
        );
        assert!(
            visited.load(Ordering::Relaxed) <= 129,
            "projection must not repeatedly prefetch complete wide pages"
        );
    }
}

fn source(table: Arc<dyn TableRead>, local: bool) -> Box<dyn RowSource> {
    let columns = vec!["id".into(), "body".into(), "missing".into()];
    if local {
        use crate::query::local_table::{LocalTableRowSource, LocalTableScanConfig};
        Box::new(
            LocalTableRowSource::new(LocalTableScanConfig {
                cancellation: uqa_core::CancellationToken::new(),
                serializable: None,
                table_name: "t".into(),
                table,
                column_definitions: Arc::new(Vec::new()),
                columns: columns.clone(),
                schema: columns.clone(),
                physical_schema: crate::RowSchema::new(columns),
                metadata: uqa_sql::plan::source_projection::RelationMetadataProjection::default(),
                table_oid: None,
                predicate: None,
                estimated_cardinality: 128,
                lock_origin: None,
                recheck_pins: None,
                candidates: None,
                command_changes: None,
            })
            .with_scan_batch_bytes(64 * 1024),
        )
    } else {
        Box::new(
            ScoredDocumentSource::new("t", table, ScoredInput::All, columns, None, None)
                .with_scan_batch_bytes(64 * 1024),
        )
    }
}

#[test]
fn shared_scan_reuses_immutable_projection_classification() {
    projection_classification(true);
}

#[test]
fn borrowed_scan_reuses_immutable_projection_classification() {
    projection_classification(false);
}

fn projection_classification(shared: bool) {
    const WIDTH: usize = 64;
    const ROWS: u64 = 512;
    let columns = (0..WIDTH)
        .map(|slot| format!("field_{slot}"))
        .collect::<Vec<_>>();
    let mut documents = MemoryDocumentStore::new();
    for id in 1..=ROWS {
        documents
            .put(
                id,
                columns
                    .iter()
                    .enumerate()
                    .map(|(slot, name)| {
                        (
                            name.clone(),
                            Value::Int((id * WIDTH as u64 + slot as u64) as i64),
                        )
                    })
                    .collect(),
            )
            .unwrap();
    }
    let visited = Arc::new(AtomicUsize::new(0));
    let table = Arc::new(Table(RwLock::new(Box::new(Documents {
        rows: documents,
        shared,
        visited: Arc::clone(&visited),
    }))));
    let before = crate::query::scored_input::projection::inspections();
    let mut source =
        ScoredDocumentSource::new("wide", table, ScoredInput::All, columns, None, None)
            .with_scan_batch_bytes(64 * 1024);
    assert!(source.supports_batched_consumption());
    let mut id = 1;
    loop {
        let rows = source.next_physical_batch(32).unwrap();
        assert!(rows.len() <= 32);
        if rows.is_empty() {
            break;
        }
        for row in rows {
            for slot in 0..WIDTH {
                assert_eq!(
                    row.value(slot),
                    Some(&Value::Int((id * WIDTH as u64 + slot as u64) as i64))
                );
            }
            id += 1;
        }
    }
    assert_eq!(id, ROWS + 1);
    assert_eq!(visited.load(Ordering::Relaxed), ROWS as usize);
    let after = crate::query::scored_input::projection::inspections();
    let inspected = (after.0 - before.0, after.1 - before.1);
    if shared {
        assert_eq!(
            inspected,
            (1, WIDTH),
            "immutable metadata and slots must be classified once per source"
        );
    } else {
        assert_eq!(
            inspected.0, 1,
            "borrowed pages must not reclassify immutable metadata"
        );
    }
}

#[test]
fn ordered_typed_scan_batches_do_not_reencode_the_complete_schema() {
    use crate::query::local_table::{LocalTableRowSource, LocalTableScanConfig};
    use uqa_sql::ColumnType;

    let columns = (0..64).map(|n| format!("value_{n}")).collect::<Vec<_>>();
    let schema = crate::RowSchema::with_types(
        columns.clone(),
        vec![Some(ColumnType::BigInteger); columns.len()],
    );
    let mut observations = Vec::new();
    for (shared, local) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut rows = MemoryDocumentStore::new();
        for id in 1..=512 {
            rows.put(
                id,
                columns
                    .iter()
                    .enumerate()
                    .map(|(slot, name)| (name.clone(), Value::Int(id as i64 * 64 + slot as i64)))
                    .collect(),
            )
            .unwrap();
        }
        let table = Arc::new(Table(RwLock::new(Box::new(Documents {
            rows,
            shared,
            visited: Arc::new(AtomicUsize::new(0)),
        }))));
        let mut scan: Box<dyn RowSource> = if local {
            Box::new(
                LocalTableRowSource::new(LocalTableScanConfig {
                    cancellation: uqa_core::CancellationToken::new(),
                    serializable: None,
                    table_name: "typed_scan".into(),
                    table,
                    column_definitions: Arc::new(Vec::new()),
                    columns: columns.clone(),
                    schema: columns.clone(),
                    physical_schema: schema.clone(),
                    metadata: uqa_sql::plan::source_projection::RelationMetadataProjection::default(
                    ),
                    table_oid: None,
                    predicate: None,
                    estimated_cardinality: 512,
                    lock_origin: None,
                    recheck_pins: None,
                    candidates: None,
                    command_changes: None,
                })
                .with_scan_batch_bytes(4096),
            )
        } else {
            let mut scan = ScoredDocumentSource::new(
                "typed_scan",
                table,
                ScoredInput::All,
                columns.clone(),
                None,
                None,
            )
            .with_scan_batch_bytes(4096);
            scan.schema = schema.clone();
            Box::new(scan)
        };
        let allocation = allocation_counter::measure(|| {
            for id in 1..=512 {
                let rows = scan.next_physical_batch(1).unwrap();
                assert_eq!(rows.len(), 1);
                for slot in 0..64 {
                    assert_eq!(
                        rows[0].value(slot),
                        Some(&Value::Int(id * 64 + slot as i64))
                    );
                }
            }
            assert!(scan.next_physical_batch(1).unwrap().is_empty());
        });
        observations.push((shared, local, allocation));
    }
    for (shared, local, allocation) in &observations {
        eprintln!("shared={shared}, local={local}: {allocation:?}");
    }
    assert!(
        observations
            .iter()
            .all(|(_, _, a)| a.count_total < 8192 && a.bytes_max < 256 * 1024),
        "{observations:?}"
    );
}

#[test]
fn mixed_width_shared_pages_do_not_prefetch_the_unconsumed_tail_repeatedly() {
    let mut documents = MemoryDocumentStore::new();
    for id in 1..=128 {
        let body = if id % 2 == 1 {
            "한".repeat(8192)
        } else {
            String::new()
        };
        documents
            .put(
                id,
                [
                    ("id".into(), Value::Int(id as i64)),
                    ("body".into(), Value::Str(body)),
                ]
                .into(),
            )
            .unwrap();
    }
    let visited = Arc::new(AtomicUsize::new(0));
    let table = Arc::new(Table(RwLock::new(Box::new(Documents {
        rows: documents,
        shared: true,
        visited: Arc::clone(&visited),
    }))));
    let mut source = source(table, false);
    let mut expected = 1;
    loop {
        let rows = source.next_physical_batch(1024).unwrap();
        if rows.is_empty() {
            break;
        }
        assert!(
            rows.len() <= 6,
            "batch must retain the existing 64KiB bound"
        );
        for row in rows {
            assert_eq!(row.value(0), Some(&Value::Int(expected)));
            assert_eq!(row.value(2), Some(&Value::Null));
            match row.value(1) {
                Some(Value::Str(body)) => {
                    assert_eq!(body.len(), if expected % 2 == 1 { 24576 } else { 0 });
                }
                value => panic!("unexpected shared payload: {value:?}"),
            }
            expected += 1;
        }
    }
    assert_eq!(expected, 129);
    assert!(visited.load(Ordering::Relaxed) <= 130,
        "a narrow last row must not inflate the next page and repeatedly read its unconsumed wide rows");
}
