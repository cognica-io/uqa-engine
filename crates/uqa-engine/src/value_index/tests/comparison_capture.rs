//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The retained table adapter captures identities before fallible index validation reads rows.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

#[derive(Clone)]
struct ComparisonRows {
    count: u64,
    captured: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    fail_page: bool,
}

impl DocumentStore for ComparisonRows {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("validation must not write rows")
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        assert!(
            self.captured.load(Ordering::Relaxed),
            "row read before identity capture completes"
        );
        assert!((1..=self.count).contains(&id));
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(Some(StoredDocument::new(Document::from([
            ("id".into(), Value::Int(i64::try_from(id).unwrap())),
            ("key".into(), Value::Null),
        ]))))
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("validation must not delete rows")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("validation must not clear rows")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.captured.store(true, Ordering::Relaxed);
        Ok((1..=self.count).collect())
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        if self.fail_page && after.is_some() {
            return Err(uqa_core::memory::MemoryError::Limit {
                required: 65_537,
                limit: 65_536,
            }
            .into());
        }
        let mut ids = BudgetedVec::new(control.memory());
        for id in (after.unwrap_or(0)..self.count)
            .take(limit)
            .map(|id| id + 1)
        {
            control.check()?;
            ids.push(id)?;
        }
        if ids.is_empty() {
            self.captured.store(true, Ordering::Relaxed);
        }
        Ok(ids)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        Ok(usize::try_from(self.count).unwrap())
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}

fn fixture(fail_page: bool) -> (crate::Engine, ComparisonRows) {
    let engine = crate::Engine::new();
    engine.sql("CREATE TABLE comparison_capture (id BIGINT, key INTEGER[]); CREATE INDEX comparison_capture_key ON comparison_capture (key); SET work_mem = '64kB'", &[]).unwrap();
    let rows = ComparisonRows {
        count: 65_536,
        captured: Arc::new(AtomicBool::new(false)),
        reads: Arc::new(AtomicUsize::new(0)),
        fail_page,
    };
    let table = engine.require_table("comparison_capture").unwrap();
    *table.document_store.write() = Box::new(rows.clone());
    crate::Engine::value_indexes_clear(&table);
    (engine, rows)
}

#[test]
fn fallible_index_validation_captures_identities_with_bounded_workspace() {
    let (engine, rows) = fixture(false);
    let definitions = engine.physical_index_definitions().unwrap();
    let document = Document::from([("id".into(), Value::Int(1)), ("key".into(), Value::Null)]);
    let allocation = allocation_counter::measure(|| {
        definitions
            .validate_key_comparisons(
                engine.constraint_execution_context(),
                "public.comparison_capture",
                &document,
                Some(1),
            )
            .unwrap();
    });
    assert_eq!(rows.reads.load(Ordering::Relaxed), 65_535);
    assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
}

#[test]
fn fallible_index_identity_capture_preserves_typed_failure_before_row_reads() {
    let (engine, rows) = fixture(true);
    let definitions = engine.physical_index_definitions().unwrap();
    let error = definitions
        .validate_key_comparisons(
            engine.constraint_execution_context(),
            "public.comparison_capture",
            &Document::from([("key".into(), Value::Null)]),
            Some(1),
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    let error = engine
        .constraint_execution_context()
        .reads
        .table_doc_id_page(
            "comparison_capture",
            Some(128),
            128,
            &StorageReadControl::with_limit(65_536),
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(rows.reads.load(Ordering::Relaxed), 0);
    assert!(!rows.captured.load(Ordering::Relaxed));
}
