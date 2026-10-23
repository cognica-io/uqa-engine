//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::{Document, MemoryDocumentStore};
use super::*;
use crate::DocumentStore;
use uqa_core::Value;

#[test]
fn paged_memory_projection_compiles_only_encountered_layouts_once() {
    let mut store = MemoryDocumentStore::new();
    for id in 1..=80 {
        store
            .put(id, Document::from([("a".into(), Value::Int(id as i64))]))
            .unwrap();
    }
    for id in 81..=2128 {
        store
            .put(
                id,
                Document::from([(format!("field_{id}"), Value::Int(id as i64))]),
            )
            .unwrap();
    }
    COMPILED.set(0);
    let fields = ["a", "missing", "a"];
    let mut prior_mapping = None;
    for after in (0..80).step_by(8) {
        let rows = store
            .next_shared_fields(Some(after), 8, &fields)
            .unwrap()
            .unwrap();
        assert_eq!(rows.len(), 8);
        for (id, row) in rows {
            let (values, slots) = row.into_parts();
            assert_eq!(values[slots[0]], Value::Int(id as i64));
            assert_eq!(slots[1], usize::MAX);
            assert_eq!(slots[0], slots[2]);
            if let Some(previous) = &prior_mapping {
                assert!(Arc::ptr_eq(previous, &slots));
            }
            prior_mapping = Some(slots);
        }
    }
    assert_eq!(COMPILED.get(), 1);
    store
        .for_each_fields_multi_ref_with_presence(
            &[80, 81, 80, 9999],
            &fields,
            &mut |id, present, values| {
                assert_eq!(present, id != 9999);
                assert_eq!(
                    values[0],
                    &if id == 80 {
                        Value::Int(80)
                    } else {
                        Value::Null
                    }
                );
                assert_eq!(values[1], &Value::Null);
                assert_eq!(values[0], values[2]);
                true
            },
        )
        .unwrap();
    assert_eq!(COMPILED.get(), 2);
    store
        .for_each_next_fields(None, 80, &fields, &mut |id, values| {
            assert_eq!(values[0], &Value::Int(id as i64));
            true
        })
        .unwrap();
    assert_eq!(COMPILED.get(), 2);
    let warmed = allocation_counter::measure(|| {
        for after in (0..80).step_by(8) {
            store
                .for_each_next_fields(Some(after), 8, &fields, &mut |id, values| {
                    assert_eq!(values[0], &Value::Int(id as i64));
                    true
                })
                .unwrap();
        }
    });
    assert!(warmed.bytes_total < 1024, "{warmed:?}");
    assert_eq!(COMPILED.get(), 2);
    assert_eq!(
        store
            .projections
            .current
            .lock()
            .as_ref()
            .unwrap()
            .layouts
            .lock()
            .len(),
        2
    );
}

#[test]
fn read_projection_reentry_releases_replaced_metadata_after_the_active_page() {
    let mut store = MemoryDocumentStore::new();
    for id in 1..=3 {
        store
            .put(
                id,
                Document::from([
                    ("a".into(), Value::Int(id as i64)),
                    ("b".into(), Value::Int(10)),
                ]),
            )
            .unwrap();
    }
    let mut previous = None;
    store
        .for_each_next_fields(None, 3, &["a"], &mut |id, values| {
            assert_eq!(values, [&Value::Int(id as i64)]);
            if id == 1 {
                let current = store.projections.current.lock();
                previous = Some(Arc::downgrade(current.as_ref().unwrap()));
                drop(current);
                let other = store
                    .get_shared_fields(&[1], &["b", "a", "b"])
                    .unwrap()
                    .unwrap();
                assert_eq!(other[0].as_ref().unwrap().indexed_values().1, [1, 0, 1]);
                assert!(previous.as_ref().unwrap().upgrade().is_some());
            }
            true
        })
        .unwrap();
    assert!(previous.unwrap().upgrade().is_none());
    assert_eq!(
        &*store.projections.current.lock().as_ref().unwrap().fields,
        ["b", "a", "b"]
    );
}

#[test]
fn memory_projection_layout_identity_survives_cow_clear_and_retained_rows() {
    let mut store = MemoryDocumentStore::new();
    store
        .put(
            1,
            Document::from([("a".into(), Value::Int(1)), ("b".into(), Value::Null)]),
        )
        .unwrap();
    let snapshot = store.shared_snapshot();
    let retained = store
        .get_shared_fields(&[1], &["a", "b", "missing"])
        .unwrap()
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    let old = snapshot
        .get_shared_fields(&[1], &["a"])
        .unwrap()
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    store.clear().unwrap();
    store
        .put(
            1,
            Document::from([("0".into(), Value::Int(7)), ("a".into(), Value::Int(9))]),
        )
        .unwrap();
    let current = store
        .get_shared_fields(&[1], &["a", "b", "missing"])
        .unwrap()
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    let (values, slots) = current.indexed_values();
    assert_eq!(slots, [1, usize::MAX, usize::MAX]);
    assert_eq!(values[slots[0]], Value::Int(9));
    let (values, slots) = retained.indexed_values();
    assert_eq!(slots, [0, 1, usize::MAX]);
    assert_eq!(values[0], Value::Int(1));
    assert_eq!(values[1], Value::Null);
    let mut writable = snapshot.clone();
    writable
        .put(1, Document::from([("a".into(), Value::Int(11))]))
        .unwrap();
    assert_eq!(snapshot.get_field(1, "a").unwrap(), Some(Value::Int(1)));
    drop((store, snapshot, writable));
    assert_eq!(old.indexed_values().0[0], Value::Int(1));
}

#[test]
fn empty_memory_projections_keep_cardinality_without_per_layout_mapping_work() {
    let mut store = MemoryDocumentStore::new();
    for id in 0..128 {
        store
            .put(id, Document::from([(format!("field_{id}"), Value::Null)]))
            .unwrap();
    }
    COMPILED.set(0);
    let rows = store.next_shared_fields(None, 128, &[]).unwrap().unwrap();
    assert_eq!(rows.len(), 128);
    assert!(rows
        .iter()
        .all(|(_, row)| row.indexed_values().1.is_empty()));
    assert_eq!(COMPILED.get(), 1);
    let selected = store
        .get_shared_fields(&[0, 99, 9999, 0], &[])
        .unwrap()
        .unwrap();
    assert!(selected[2].is_none());
    assert_eq!(selected.iter().filter(|row| row.is_some()).count(), 3);
    assert_eq!(COMPILED.get(), 1);
}
