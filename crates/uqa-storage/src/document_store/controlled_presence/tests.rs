//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    MemoryDocumentStore, ReadOnlySnapshot, RetainedDocumentPage, RetainedDocumentStoreBuilder,
    StoredDocument,
};
use std::sync::Arc;
use uqa_core::Value;

#[derive(Clone, Copy)]
enum Mode {
    Rows,
    Unsupported,
    Foreign,
    Short,
    Long,
    Cancel,
}

struct Probe {
    rows: MemoryDocumentStore,
    mode: Mode,
    foreign: StorageReadControl,
}

impl DocumentStore for Probe {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("field metadata must not use legacy owned rows")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable probe")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("field metadata must not enumerate the corpus")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("count")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacement view")
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDocumentPage> {
        if matches!(self.mode, Mode::Unsupported) {
            return Err(StorageBackendError::Other(
                "controlled rows unavailable".into(),
            ));
        }
        self.rows.get_stored_many_controlled(ids, control)
    }
    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<bool>> {
        if matches!(self.mode, Mode::Rows | Mode::Unsupported) {
            return from_controlled_rows(self, ids, fields, control);
        }
        let mut page = BudgetedVec::new(if matches!(self.mode, Mode::Foreign) {
            self.foreign.memory()
        } else {
            control.memory()
        });
        let expected = ids.len() * fields.len();
        let count = match self.mode {
            Mode::Short => expected - 1,
            Mode::Long => expected + 1,
            _ => expected,
        };
        for _ in 0..count {
            page.push(true)?;
        }
        if matches!(self.mode, Mode::Cancel) {
            control.cancellation().cancel();
        }
        Ok(page)
    }
}

fn rows() -> MemoryDocumentStore {
    let mut rows = MemoryDocumentStore::new();
    rows.put(
        3,
        [
            ("large".into(), Value::Str("x".repeat(256 << 10))),
            ("null".into(), Value::Null),
        ]
        .into(),
    )
    .unwrap();
    rows
}

#[test]
fn field_metadata_distinguishes_null_missing_rows_and_duplicates_without_copying_payloads() {
    let rows = rows();
    let owner = StorageReadControl::with_limit(1 << 20);
    let mut builder = RetainedDocumentStoreBuilder::new(&owner);
    builder
        .add_document(3, rows.get_stored(3).unwrap().unwrap())
        .unwrap();
    let retained = builder.finish().unwrap();
    let nested = ReadOnlySnapshot::new(retained.snapshot().unwrap())
        .snapshot()
        .unwrap();
    let baseline = owner.memory().used();
    let caller = StorageReadControl::with_limit(256);
    for source in [&rows as &dyn DocumentStore, &retained, nested.as_ref()] {
        let page = read_field_presence(
            source,
            &[3, 99, 3],
            &["null", "missing", "large", "null"],
            &caller,
        )
        .unwrap();
        assert_eq!(
            &*page,
            &[true, false, true, true, false, false, false, false, true, false, true, true]
        );
        assert!(caller.memory().used() > 0);
        assert_eq!(owner.memory().used(), baseline);
        drop(page);
        assert_eq!(caller.memory().used(), 0);
    }
    drop(retained);
    assert_eq!(owner.memory().used(), baseline);
    owner.cancellation().cancel();
    assert!(matches!(
        read_field_presence(nested.as_ref(), &[3], &["null"], &caller),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(caller.memory().used(), 0);
    drop(nested);
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn default_field_metadata_keeps_controlled_row_payloads_live_until_result_admission() {
    let source = Probe {
        rows: rows(),
        mode: Mode::Rows,
        foreign: StorageReadControl::with_limit(1 << 20),
    };
    let control = StorageReadControl::with_limit(1 << 20);
    let rows = source.get_stored_many_controlled(&[3], &control).unwrap();
    let row_bytes = control.memory().used();
    drop(rows);
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - row_bytes)
        .unwrap();
    assert!(matches!(
        read_field_presence(&source, &[3], &["null"], &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    let page = read_field_presence(&source, &[99, 3], &["null", "missing"], &control).unwrap();
    assert_eq!(&*page, &[false, false, true, false]);
    assert!(control.memory().used() < row_bytes);
    drop(page);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn field_metadata_rejects_foreign_or_malformed_pages_and_preserves_final_cancellation() {
    for mode in [
        Mode::Foreign,
        Mode::Short,
        Mode::Long,
        Mode::Cancel,
        Mode::Unsupported,
    ] {
        let source = Probe {
            rows: rows(),
            mode,
            foreign: StorageReadControl::with_limit(256),
        };
        let control = StorageReadControl::with_limit(256);
        let result = read_field_presence(&source, &[3, 99], &["null", "large"], &control);
        if matches!(mode, Mode::Cancel) {
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        } else {
            assert!(matches!(result, Err(StorageBackendError::Other(_))));
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(source.foreign.memory().used(), 0);
    }
}

#[test]
fn empty_field_metadata_checks_cancellation_and_quota_failure_releases_partial_flags() {
    let source = Probe {
        rows: rows(),
        mode: Mode::Unsupported,
        foreign: StorageReadControl::with_limit(0),
    };
    let control = StorageReadControl::with_limit(0);
    assert!(read_field_presence(&source, &[], &["null"], &control)
        .unwrap()
        .is_empty());
    assert!(read_field_presence(&source, &[3], &[], &control)
        .unwrap()
        .is_empty());
    assert!(matches!(
        read_field_presence(&source.rows, &[3], &["null"], &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        read_field_presence(&source, &[], &[], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
}
