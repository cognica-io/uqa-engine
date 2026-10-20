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
