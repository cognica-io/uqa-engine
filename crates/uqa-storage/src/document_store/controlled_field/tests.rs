//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{DocumentMetadata, MemoryDocumentStore, StoredDocument};
use std::{collections::BTreeMap, sync::Arc};

struct Source {
    rows: MemoryDocumentStore,
    malformed: u8,
    cancel_after: Option<uqa_core::CancellationToken>,
}

impl Source {
    fn new() -> Self {
        let mut rows = MemoryDocumentStore::new();
        rows.put_stored(
            1,
            StoredDocument::with_metadata(
                BTreeMap::from([
                    ("small".into(), Value::Str("selected".into())),
                    ("large".into(), Value::Str("x".repeat(8192))),
                    ("null".into(), Value::Null),
                ]),
                DocumentMetadata::with_tuple_xmin(41),
            ),
        )
        .unwrap();
        Self {
            rows,
            malformed: 0,
            cancel_after: None,
        }
    }
}

impl DocumentStore for Source {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("read only")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("uncontrolled row copy")
    }
    fn get_field(&self, _: DocId, _: &str) -> StorageBackendResult<Option<Value>> {
        panic!("uncontrolled field copy")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("read only")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("read only")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("unrelated enumeration")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("already selected source")
    }
    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<bool>> {
        self.rows.field_presence_controlled(ids, fields, control)
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        match self.malformed {
            0 => self
                .rows
                .for_each_fields_multi_ref_with_presence(ids, fields, visitor)?,
            1 => {
                visitor(99, true, &[&Value::Null]);
            }
            2 => {
                visitor(ids[0], false, &[&Value::Null]);
            }
            3 => {
                visitor(ids[0], true, &[]);
            }
            4 => {}
            5 => {
                visitor(ids[0], true, &[&Value::Null]);
                visitor(ids[0], true, &[&Value::Null]);
            }
            _ => unreachable!(),
        }
        if let Some(cancellation) = &self.cancel_after {
            cancellation.cancel();
        }
        Ok(())
    }
}

#[test]
fn selected_null_and_absent_fields_need_no_projection_allocation() {
    let source = Source::new();
    let retained_control = StorageReadControl::with_limit(64 << 10);
    let mut builder = crate::RetainedDocumentStoreBuilder::new(&retained_control);
    builder
        .add_document(1, source.rows.get_stored(1).unwrap().unwrap())
        .unwrap();
    let retained: Arc<dyn DocumentStore> = Arc::new(builder.finish().unwrap());
    let memory = source.rows.snapshot().unwrap();
    let wrapped: Arc<dyn DocumentStore> =
        Arc::new(crate::ReadOnlySnapshot::new(Arc::clone(&retained)));
    let control = StorageReadControl::with_limit(0);
    for source in [memory, retained, wrapped] {
        assert_eq!(
            *read_selected_field(source.as_ref(), 1, "null", &control)
                .unwrap()
                .unwrap(),
            Value::Null
        );
        for (id, field) in [(1, "absent"), (99, "large")] {
            assert!(read_selected_field(source.as_ref(), id, field, &control)
                .unwrap()
                .is_none());
        }
        assert!(matches!(
            read_selected_field(source.as_ref(), 1, "small", &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(retained_control.memory().used(), 0);
}

#[test]
fn selected_field_copies_distinguish_absence_and_null_without_copying_other_fields() {
    let source = Source::new();
    let control = StorageReadControl::with_limit(1024);
    let selected = read_selected_field(&source, 1, "small", &control)
        .unwrap()
        .unwrap();
    assert_eq!(*selected, Value::Str("selected".into()));
    assert!(control.memory().used() >= "selected".len());
    assert_eq!(
        *read_selected_field(&source, 1, "null", &control)
            .unwrap()
            .unwrap(),
        Value::Null
    );
    assert!(read_selected_field(&source, 1, "absent", &control)
        .unwrap()
        .is_none());
    assert!(read_selected_field(&source, 99, "large", &control)
        .unwrap()
        .is_none());
    assert!(matches!(
        read_selected_field(&source, 1, "large", &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(*selected, Value::Str("selected".into()));
    drop(selected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn selected_field_copy_keeps_the_first_failure_and_releases_cancelled_output() {
    let mut source = Source::new();
    let control = StorageReadControl::with_limit(1024);
    source.cancel_after = Some(control.cancellation().clone());
    assert!(matches!(
        read_selected_field(&source, 1, "small", &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().reset();
    assert!(matches!(
        read_selected_field(&source, 1, "large", &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(control.cancellation().is_cancelled());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn selected_field_copy_rejects_inconsistent_provider_rows() {
    let control = StorageReadControl::with_limit(1024);
    for malformed in 1..=5 {
        let mut source = Source::new();
        source.malformed = malformed;
        assert!(
            matches!(
                read_selected_field(&source, 1, "small", &control),
                Err(StorageBackendError::Other(_))
            ),
            "malformed {malformed}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}
