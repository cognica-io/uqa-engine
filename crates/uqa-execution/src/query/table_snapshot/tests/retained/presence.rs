//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{document_store::read_field_presence, StorageBackendError};

fn assert_shared_source(documents: &dyn DocumentStore, expected: *const Value) {
    let shared = documents
        .get_shared_fields(&[1], &["renamed"])
        .unwrap()
        .unwrap();
    shared[0].as_ref().unwrap().with_projected(|values| {
        assert_eq!(values.len(), 1);
        assert!(std::ptr::eq(values[0], expected));
    });
    drop(shared);
    assert!(documents
        .get_shared_fields(&[2], &["renamed"])
        .unwrap()
        .is_none());
}

#[test]
fn ordinary_defaults_and_renames_borrow_payloads_without_reentering_provider_guards() {
    let columns = columns("CREATE TABLE t (old TEXT, plain TEXT)");
    let mut target = columns.clone();
    target[0].name = "renamed".into();
    for column in &mut target {
        column.missing_value = Some(Value::Str("default".repeat(32 << 10)));
    }
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let selected = schema(&target, &index);
    let defaults = [
        selected.columns[0].missing_value.as_ref().unwrap(),
        selected.columns[1].missing_value.as_ref().unwrap(),
    ];
    let mut rows = MemoryDocumentStore::new();
    for (id, fields) in [
        (1, vec![("old", Value::Str("stored".repeat(32 << 10)))]),
        (
            2,
            vec![
                ("old", Value::Null),
                ("renamed", Value::Str("masked".repeat(32 << 10))),
                ("plain", Value::Null),
            ],
        ),
        (
            3,
            vec![("renamed", Value::Str("alternate".repeat(32 << 10)))],
        ),
        (4, vec![]),
    ] {
        rows.put_stored(id, document(&fields, 41)).unwrap();
    }
    let mut source = ProjectedSource::new(&rows);
    source.forbid_owned_fields = true;
    let mut addresses = Vec::new();
    source
        .rows
        .for_each_fields_multi_ref_with_presence(
            &[1, 3],
            &["old", "renamed"],
            &mut |id, _, values| {
                addresses.push(std::ptr::from_ref(values[usize::from(id == 3)]));
                true
            },
        )
        .unwrap();
    let allowance = StorageReadControl::with_limit(16 << 10);
    let view = retain(
        Arc::new(source),
        &columns,
        &selected,
        DocumentChanges::default(),
        &allowance,
    )
    .unwrap();
    rows.clear().unwrap();
    drop(rows);
    let retained_bytes = allowance.memory().used();
    assert_shared_source(view.documents.as_ref(), addresses[0]);
    let mut visited = 0;
    let ids = [1, 2, 3, 4, 99, 1];
    view.documents
        .for_each_fields_multi_ref_with_presence(
            &ids,
            &["renamed", "plain", "renamed"],
            &mut |id, present, values| {
                assert_eq!(id, ids[visited]);
                visited += 1;
                assert_eq!(present, id != 99);
                match id {
                    1 => assert!(std::ptr::eq(values[0], addresses[0])),
                    2 | 99 => assert_eq!(*values[0], Value::Null),
                    3 => assert!(std::ptr::eq(values[0], addresses[1])),
                    4 => assert!(std::ptr::eq(values[0], defaults[0])),
                    _ => unreachable!(),
                }
                if matches!(id, 1 | 3 | 4) {
                    assert!(std::ptr::eq(values[1], defaults[1]));
                } else {
                    assert_eq!(*values[1], Value::Null);
                }
                assert_eq!(values[0], values[2]);
                assert!(allowance.memory().used() > retained_bytes);
                true
            },
        )
        .unwrap();
    assert_eq!(visited, ids.len());
    assert_eq!(allowance.memory().used(), retained_bytes);
    let mut stopped = 0;
    view.documents
        .for_each_fields_multi_ref_with_presence(&ids, &["renamed"], &mut |_, _, _| {
            stopped += 1;
            false
        })
        .unwrap();
    assert_eq!(stopped, 1);
    drop(view);
    assert_eq!(allowance.memory().used(), 0);
}

#[test]
fn nested_field_metadata_preserves_private_masks_without_evaluating_generated_columns() {
    let columns =
        columns("CREATE TABLE t (old INT, bad INT GENERATED ALWAYS AS (1 / old) VIRTUAL)");
    let mut target = columns.clone();
    target[0].name = "renamed".into();
    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(
        0,
        document(&[("old", Value::Int(0)), ("extra", Value::Null)], 41),
    )
    .unwrap();
    rows.put_stored(3, document(&[("old", Value::Int(0))], 41))
        .unwrap();
    let mut private = MemoryDocumentStore::new();
    private
        .put_stored(
            2,
            document(&[("renamed", Value::Null), ("extra", Value::Null)], 42),
        )
        .unwrap();
    let owner = control();
    let changes = DocumentChanges::default()
        .with_retained(
            Arc::new(ProjectedSource::new(&private)),
            selection([(2, true), (3, false)]),
            &owner,
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(ProjectedSource::new(&rows)),
        &columns,
        &schema(&target, &index),
        changes,
        &owner,
    )
    .unwrap();
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    drop(view);
    rows.clear().unwrap();
    private.clear().unwrap();
    let baseline = owner.memory().used();
    let caller = StorageReadControl::with_limit(4096);
    let page = read_field_presence(
        nested.as_ref(),
        &[0, 2, 3, 99, 0],
        &["renamed", "bad", "old", "extra", "unknown"],
        &caller,
    )
    .unwrap();
    assert_eq!(
        &*page,
        &[
            true, true, false, true, false, true, true, false, true, false, false, false, false,
            false, false, false, false, false, false, false, true, true, false, true, false,
        ]
    );
    assert_eq!(owner.memory().used(), baseline);
    drop(page);
    assert_eq!(caller.memory().used(), 0);
    owner.cancellation().cancel();
    assert!(matches!(
        read_field_presence(nested.as_ref(), &[0], &["bad"], &caller),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(caller.memory().used(), 0);
    owner.cancellation().reset();
    caller.cancellation().cancel();
    assert!(matches!(
        read_field_presence(nested.as_ref(), &[0], &["bad"], &caller),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(nested);
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn projection_quota_failure_and_cancellation_preserve_the_selected_source() {
    let columns = columns("CREATE TABLE t (old TEXT)");
    let mut target = columns.clone();
    target[0].missing_value = Some(Value::Str("missing".repeat(1024)));
    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(1, document(&[], 41)).unwrap();
    let mut source = ProjectedSource::new(&rows);
    source.forbid_owned_fields = true;
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let owner = control();
    let view = retain(
        Arc::new(source),
        &columns,
        &schema(&target, &index),
        DocumentChanges::default(),
        &owner,
    )
    .unwrap();
    let baseline = owner.memory().used();
    let full = owner
        .memory()
        .reserve(owner.memory().limit() - baseline)
        .unwrap();
    let mut visited = false;
    assert!(matches!(
        view.documents
            .for_each_fields_multi_ref_with_presence(&[1], &["old"], &mut |_, _, _| {
                visited = true;
                true
            }),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(!visited);
    drop(full);
    assert_eq!(owner.memory().used(), baseline);
    owner.cancellation().cancel();
    assert!(matches!(
        view.documents
            .for_each_fields_multi_ref_with_presence(&[1], &["old"], &mut |_, _, _| {
                visited = true;
                true
            }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(!visited);
    owner.cancellation().reset();
    view.documents
        .for_each_fields_multi_ref_with_presence(&[1], &["old"], &mut |_, present, values| {
            assert!(present);
            assert_eq!(values[0], target[0].missing_value.as_ref().unwrap());
            visited = true;
            false
        })
        .unwrap();
    assert!(visited);
    assert_eq!(owner.memory().used(), baseline);
    drop(view);
    assert_eq!(owner.memory().used(), 0);
}
