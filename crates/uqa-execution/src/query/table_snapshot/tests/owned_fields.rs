//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn selected() -> (StorageReadControl, MaterializedTable) {
    let columns = columns("CREATE TABLE t (a TEXT, b TEXT)");
    let mut rows = MemoryDocumentStore::new();
    let row = document(
        &[
            ("a", Value::Str("a".repeat(4096))),
            ("b", Value::Str("b".repeat(4096))),
        ],
        41,
    );
    rows.put_stored(1, row.clone()).unwrap();
    rows.put_stored(3, row.clone()).unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let changes =
        DocumentChanges::from_rows(BTreeMap::from([(2, Some(row)), (3, None)]), &control).unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        rows.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        changes,
        &control,
    )
    .unwrap();
    rows.clear().unwrap();
    (control, view)
}

#[test]
fn owned_field_and_batch_production_share_the_original_allowance() {
    let (control, view) = selected();
    let nested = view.documents.snapshot().unwrap();
    let baseline = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - baseline - (6 << 10))
        .unwrap();
    for rows in [view.documents.as_ref(), nested.as_ref()] {
        for id in [1, 2] {
            assert_eq!(
                rows.get_field(id, "a").unwrap(),
                Some(Value::Str("a".repeat(4096)))
            );
            assert!(matches!(
                rows.get_fields_multi(&[id], &["a", "b"]),
                Err(StorageBackendError::Memory(_))
            ));
        }
        assert!(matches!(
            rows.get_fields_multi(&[2, 1], &["a"]),
            Err(StorageBackendError::Memory(_))
        ));
        let duplicates = rows.get_fields_multi(&[1, 1, 3, 99], &["a"]).unwrap();
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[&1], [Value::Str("a".repeat(4096))]);
        assert!(rows.get_field(3, "a").unwrap().is_none());
        assert!(rows.get_field(99, "a").unwrap().is_none());
        assert_eq!(control.memory().used(), baseline + occupied.bytes());
    }
    drop(occupied);
    let rows = nested.get_fields_multi(&[2, 1], &["a", "b"]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(control.memory().used(), baseline);
    drop(view);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn owned_projection_callbacks_retain_each_output_and_release_it_on_stop_or_cancellation() {
    let (control, view) = selected();
    let baseline = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - baseline - (6 << 10))
        .unwrap();
    let mut ids = Vec::new();
    view.documents
        .for_each_fields_multi(&[2, 1, 3, 99, 2], &["a"], &mut |id, values| {
            ids.push(id);
            if id == 3 {
                assert_eq!(values, [Value::Null]);
            } else {
                assert_eq!(values, [Value::Str("a".repeat(4096))]);
                assert!(control.memory().used() >= baseline + occupied.bytes() + 4096);
            }
            ids.len() < 3
        })
        .unwrap();
    assert_eq!(ids, [2, 1, 3]);
    assert_eq!(control.memory().used(), baseline + occupied.bytes());
    let error = view
        .documents
        .for_each_fields_multi(&[1, 2], &["a"], &mut |_, _| {
            control.cancellation().cancel();
            false
        })
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert_eq!(control.memory().used(), baseline + occupied.bytes());
    control.cancellation().reset();
    drop(occupied);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
