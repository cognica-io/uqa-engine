//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{ScoredDocumentSource, ScoredInput};
use crate::query::table_read::TableRead;
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, DocumentStore, MemoryDocumentStore};

struct Table {
    documents: RwLock<Box<dyn DocumentStore>>,
    guard: RwLock<()>,
    value: Value,
    reads: AtomicUsize,
    fail_after: bool,
}

impl TableRead for Table {
    fn column_definitions(&self) -> Vec<uqa_sql::ast::ColumnDef> {
        Vec::new()
    }
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.documents.read()
    }
    fn maps_integer_keys(&self) -> bool {
        false
    }
    fn for_each_indexed_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> Result<Option<usize>, SQLError> {
        assert_eq!(fields, ["key"]);
        let _guard = self.guard.read();
        let mut count = 0;
        for &id in ids {
            control
                .check()
                .map_err(|error| crate::storage_errors::storage_error("fixture", &error))?;
            count += 1;
            self.reads.fetch_add(1, Ordering::Relaxed);
            if !visitor(id, true, &[&self.value]) {
                break;
            }
        }
        if self.fail_after {
            return Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "late read".into(),
            });
        }
        Ok(Some(count))
    }
}

fn fixture(fail_after: bool) -> (Arc<Table>, ScoredDocumentSource) {
    let table = Arc::new(Table {
        documents: RwLock::new(Box::new(MemoryDocumentStore::new())),
        guard: RwLock::new(()),
        value: Value::Str("x".repeat(8192)),
        reads: AtomicUsize::new(0),
        fail_after,
    });
    let source = ScoredDocumentSource::new(
        "indexed",
        table.clone(),
        ScoredInput::All,
        vec!["key".into()],
        None,
        None,
    )
    .with_scan_batch_bytes(64 * 1024)
    .with_index_only(true);
    (table, source)
}

#[test]
fn index_only_reentrant_projection_captures_all_reads_with_bounded_payload() {
    let (table, source) = fixture(false);
    let ids = (0..256).collect::<Vec<_>>();
    let mut visited = 0;
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            source
                .for_each_indexed_entry(&ids, &["key"], false, &mut |id, exists, values| {
                    assert!(
                        table.guard.try_write().is_some(),
                        "index guard retained across consumer"
                    );
                    assert_eq!(table.reads.load(Ordering::Relaxed), ids.len());
                    assert_eq!(id, ids[visited]);
                    assert!(exists);
                    assert_eq!(values, [&table.value]);
                    visited += 1;
                    true
                })
                .unwrap(),
            Some(ids.len())
        );
    });
    assert_eq!(visited, 256);
    assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
}

#[test]
fn index_only_reentrant_projection_keeps_late_read_errors_before_consumers() {
    let (_, source) = fixture(true);
    let error = source
        .for_each_indexed_entry(&[u64::MAX, 1], &["key"], false, &mut |_, _, _| {
            panic!("all reads must finish before invoking a reentrant consumer")
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
}
