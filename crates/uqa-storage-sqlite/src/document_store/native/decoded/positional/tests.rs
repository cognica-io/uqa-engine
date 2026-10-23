//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::fixture;
use super::*;
use crate::document_store::decoded_columns::DecodedColumns;
use uqa_core::Value;
use uqa_storage::{document_store::Document, read_control::StorageReadControl, DocumentStore};

#[test]
fn positional_compound_failures_keep_field_order_independent_of_projection_order() {
    let store = fixture();
    let snapshot = store.conn.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let read = NativeDocumentRead::with_control(&snapshot, "docs", &control, None).unwrap();
    let body = br#"{"a":{"$uqa_type":"value_blob","encoding":"f64_list","field":"wrong-a"},"b":{"$uqa_type":"value_blob","encoding":"f64_list","field":"wrong-b"}}"#;
    let row = [
        ValueRef::Null,
        ValueRef::Integer(1),
        ValueRef::Text(body),
        ValueRef::Null,
    ];
    let before = control.memory().used();
    let mut errors = Vec::new();
    for fields in [["a", "b"], ["b", "a"]] {
        let decoded = read.decode_positional_body(1, &row, &fields).unwrap();
        errors.push(
            read.hydrate_positional(1, decoded, &fields)
                .unwrap_err()
                .to_string(),
        );
        assert_eq!(control.memory().used(), before);
    }
    assert_eq!(errors[0], errors[1]);
    assert!(errors[0].contains("different blob field"), "{}", errors[0]);
}

#[test]
fn native_positional_body_borrows_names_and_distinguishes_missing_from_null() {
    let store = fixture();
    let snapshot = store.conn.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let read = NativeDocumentRead::with_control(&snapshot, "docs", &control, None).unwrap();
    let retained = control.memory().used();
    let name = "a".repeat(32 << 10);
    let body = format!(
        r#"{{"{name}":1,"null":null,"unused":"{}"}}"#,
        "x".repeat(64 << 10)
    );
    let fields = [&name[..], "null", "missing", &name];
    let row = [
        ValueRef::Null,
        ValueRef::Integer(1),
        ValueRef::Text(body.as_bytes()),
        ValueRef::Null,
    ];
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(read.decode_positional_body(1, &row, &fields).unwrap());
    });
    let result = result.unwrap();
    assert!(allocated.bytes_total < 4096, "{allocated:?}");
    assert_eq!(result.get(0), Some(&Value::Int(1)));
    assert_eq!(result.get(1), Some(&Value::Null));
    assert_eq!(result.get(2), None);
    assert!(std::ptr::eq(result.get(0).unwrap(), result.get(3).unwrap()));
    drop(result);
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn native_positional_pages_rebind_cache_columns_when_requested_order_changes() {
    let mut store = fixture();
    for id in 1..=3 {
        store
            .put(
                id,
                Document::from([
                    ("a".into(), Value::Int(id as i64)),
                    ("b".into(), Value::Str(format!("row-{id}"))),
                ]),
            )
            .unwrap();
    }
    let snapshot = store.conn.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let cache = DecodedColumns::new(64 << 10);
    let read = NativeDocumentRead::with_control(&snapshot, "docs", &control, Some(&cache)).unwrap();
    let initial = control.memory().used();
    assert_eq!(
        read.visit_borrowed_fields(None, 2, &["a", "b", "missing", "a"], &mut |id, values| {
            assert_eq!(values[0], &Value::Int(id as i64));
            assert_eq!(values[1], &Value::Str(format!("row-{id}")));
            assert!(std::ptr::eq(values[0], values[3]));
            true
        })
        .unwrap(),
        2
    );
    assert_eq!(
        read.visit_borrowed_fields(
            Some(2),
            2,
            &["b", "missing", "a", "b"],
            &mut |id, values| {
                assert_eq!(id, 3);
                assert_eq!(values[0], &Value::Str("row-3".into()));
                assert_eq!(values[1], &Value::Null);
                assert_eq!(values[2], &Value::Int(3));
                assert!(std::ptr::eq(values[0], values[3]));
                true
            }
        )
        .unwrap(),
        1
    );
    assert_eq!(control.memory().used(), initial);
    let mut cached = None;
    snapshot
        .read_latest_documents("docs", read.owner.unwrap(), &control, &mut |latest| {
            cached = cache.get(
                "docs",
                read.owner.unwrap(),
                latest.generation(),
                &["b", "a", "missing", "b"],
            );
            Ok(())
        })
        .unwrap()
        .unwrap();
    let (ids, columns) = cached.unwrap();
    assert_eq!(*ids, [1, 2, 3]);
    assert_eq!(
        *columns[0],
        [
            Value::Str("row-1".into()),
            Value::Str("row-2".into()),
            Value::Str("row-3".into())
        ]
    );
    assert_eq!(*columns[1], [Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(*columns[2], [Value::Null, Value::Null, Value::Null]);
    assert!(std::sync::Arc::ptr_eq(&columns[0], &columns[3]));
    drop((ids, columns));
    assert_eq!(control.memory().used(), initial);
}
