//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::projection::should_merge_projected_scan;
use super::*;

fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn put_get_round_trip() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("title", Value::Str("rust".into()))]))
        .unwrap();
    let got = s.get(1).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
}

#[test]
fn get_field_returns_value() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("year", Value::Int(2026))])).unwrap();
    assert_eq!(s.get_field(1, "year").unwrap(), Some(Value::Int(2026)));
    assert_eq!(s.get_field(1, "missing").unwrap(), None);
    assert_eq!(s.get_field(99, "year").unwrap(), None);
}

#[test]
fn delete_removes_doc() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("a", Value::Int(1))])).unwrap();
    s.delete(1).unwrap();
    assert!(s.get(1).unwrap().is_none());
    assert_eq!(s.len().unwrap(), 0);
}

#[test]
fn doc_ids_returns_all() {
    let mut s = MemoryDocumentStore::new();
    s.put(2, Document::new()).unwrap();
    s.put(1, Document::new()).unwrap();
    s.put(3, Document::new()).unwrap();
    assert_eq!(s.doc_ids().unwrap(), vec![1, 2, 3]);
}

#[test]
fn get_fields_bulk_returns_value_per_id_with_null_for_missing() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("year", Value::Int(2026))])).unwrap();
    s.put(2, doc([("year", Value::Int(2025))])).unwrap();
    let got = s.get_fields_bulk(&[1, 2, 99], "year").unwrap();
    assert_eq!(got.get(&1), Some(&Value::Int(2026)));
    assert_eq!(got.get(&2), Some(&Value::Int(2025)));
    assert_eq!(got.get(&99), Some(&Value::Null));
}

#[test]
fn get_fields_multi_projects_in_requested_order() {
    let mut s = MemoryDocumentStore::new();
    s.put(
        1,
        doc([
            ("year", Value::Int(2026)),
            ("title", Value::Str("rust".into())),
            ("unused", Value::Bool(true)),
        ]),
    )
    .unwrap();

    let got = s
        .get_fields_multi(&[1, 99], &["title", "missing", "year"])
        .unwrap();
    assert_eq!(
        got.get(&1),
        Some(&vec![
            Value::Str("rust".into()),
            Value::Null,
            Value::Int(2026)
        ])
    );
    assert!(!got.contains_key(&99));
}

#[test]
fn for_each_fields_multi_streams_doc_id_order_and_can_stop() {
    let mut s = MemoryDocumentStore::new();
    s.put(2, doc([("value", Value::Int(20))])).unwrap();
    s.put(1, doc([("value", Value::Int(10))])).unwrap();

    let mut visited = Vec::new();
    s.for_each_fields_multi(&[2, 99, 1], &["value"], &mut |doc_id, values| {
        visited.push((doc_id, values));
        doc_id != 99
    })
    .unwrap();

    assert_eq!(
        visited,
        vec![(2, vec![Value::Int(20)]), (99, vec![Value::Null])]
    );
}

#[test]
fn for_each_fields_multi_ref_borrows_memory_values_and_reuses_nulls() {
    let mut s = MemoryDocumentStore::new();
    s.put(2, doc([("value", Value::Str("twenty".into()))]))
        .unwrap();
    let stored = std::ptr::from_ref(s.field(&s.documents[&2], "value").unwrap());

    let mut visited = Vec::new();
    s.for_each_fields_multi_ref(&[2, 99], &["value"], &mut |doc_id, values| {
        if doc_id == 2 {
            assert_eq!(std::ptr::from_ref(values[0]), stored);
        }
        visited.push((doc_id, values[0].clone()));
        true
    })
    .unwrap();

    assert_eq!(
        visited,
        vec![(2, Value::Str("twenty".into())), (99, Value::Null),]
    );
}

#[test]
fn for_each_fields_multi_ref_preserves_projection_order_across_scan_paths() {
    let mut s = MemoryDocumentStore::new();
    for doc_id in 1..=10 {
        s.put(
            doc_id,
            doc([
                ("alpha", Value::Int(doc_id as i64)),
                ("middle", Value::Bool(true)),
                ("zulu", Value::Int((doc_id * 10) as i64)),
            ]),
        )
        .unwrap();
    }

    let mut dense = Vec::new();
    s.for_each_fields_multi_ref(
        &[2, 3, 4, 99],
        &["alpha", "missing", "zulu"],
        &mut |doc_id, values| {
            dense.push((
                doc_id,
                values.iter().map(|value| (*value).clone()).collect(),
            ));
            true
        },
    )
    .unwrap();
    assert_eq!(
        dense,
        vec![
            (2, vec![Value::Int(2), Value::Null, Value::Int(20)]),
            (3, vec![Value::Int(3), Value::Null, Value::Int(30)]),
            (4, vec![Value::Int(4), Value::Null, Value::Int(40)]),
            (99, vec![Value::Null, Value::Null, Value::Null]),
        ]
    );

    let mut unsorted = Vec::new();
    s.for_each_fields_multi_ref(&[10, 1], &["zulu", "alpha"], &mut |doc_id, values| {
        unsorted.push((
            doc_id,
            values.iter().map(|value| (*value).clone()).collect(),
        ));
        true
    })
    .unwrap();
    assert_eq!(
        unsorted,
        vec![
            (10, vec![Value::Int(100), Value::Int(10)]),
            (1, vec![Value::Int(10), Value::Int(1)]),
        ]
    );
}

#[test]
fn projected_scan_uses_merge_only_for_dense_or_table_wide_id_ranges() {
    assert!(should_merge_projected_scan(&[100, 101, 102, 103], 20_000));
    assert!(should_merge_projected_scan(&[1, 10_000], 10));
    assert!(!should_merge_projected_scan(&[1, 10_000], 20_000));
    assert!(!should_merge_projected_scan(&[2, 1], 2));
    assert!(!should_merge_projected_scan(&[1], 1));
}

#[test]
fn for_each_fields_multi_ref_uses_each_documents_layout() {
    let mut s = MemoryDocumentStore::new();
    s.put(
        1,
        doc([("alpha", Value::Int(1)), ("zulu", Value::Str("one".into()))]),
    )
    .unwrap();
    s.put(
        2,
        doc([
            ("alpha", Value::Int(2)),
            ("middle", Value::Str("two".into())),
        ]),
    )
    .unwrap();

    let mut projected = Vec::new();
    s.for_each_fields_multi_ref(&[1, 2], &["alpha", "zulu"], &mut |doc_id, values| {
        projected.push((
            doc_id,
            values.iter().map(|value| (*value).clone()).collect(),
        ));
        true
    })
    .unwrap();
    assert_eq!(
        projected,
        vec![
            (1, vec![Value::Int(1), Value::Str("one".into())]),
            (2, vec![Value::Int(2), Value::Null]),
        ]
    );

    s.patch_fields(
        2,
        &BTreeMap::from([
            ("middle".to_string(), Value::Null),
            ("zulu".to_string(), Value::Str("patched".into())),
        ]),
    )
    .unwrap();
    let mut patched = Vec::new();
    s.for_each_fields_multi_ref(&[2], &["alpha", "zulu"], &mut |_doc_id, values| {
        patched.extend(values.iter().map(|value| (*value).clone()));
        true
    })
    .unwrap();
    assert_eq!(patched, vec![Value::Int(2), Value::Str("patched".into())]);
}

#[test]
fn shared_projection_reuses_the_stored_value_vector() {
    let mut s = MemoryDocumentStore::new();
    s.put(
        1,
        doc([
            ("alpha", Value::Str("kept".into())),
            ("zulu", Value::Int(9)),
        ]),
    )
    .unwrap();
    let stored = Arc::clone(&s.documents[&1].values);

    let mut rows = s
        .get_shared_fields(&[1], &["zulu", "missing", "alpha"])
        .unwrap()
        .unwrap();
    let shared = rows.pop().unwrap().unwrap();
    let projected = shared.with_projected(|values| {
        values
            .iter()
            .map(|value| (*value).clone())
            .collect::<Vec<_>>()
    });
    let (values, _) = shared.into_parts();

    assert!(Arc::ptr_eq(&values, &stored));
    assert_eq!(
        projected,
        vec![Value::Int(9), Value::Null, Value::Str("kept".into())]
    );
}

#[test]
fn shared_cursor_combines_id_scan_and_projection() {
    let mut store = MemoryDocumentStore::new();
    store
        .put(1, doc([("alpha", Value::Int(1)), ("zulu", Value::Int(2))]))
        .unwrap();
    store
        .put(3, doc([("alpha", Value::Int(3)), ("zulu", Value::Int(4))]))
        .unwrap();

    let rows = store
        .next_shared_fields(Some(1), 1, &["zulu", "alpha"])
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 3);
    assert_eq!(
        rows[0].1.with_projected(|values| {
            values
                .iter()
                .map(|value| (*value).clone())
                .collect::<Vec<_>>()
        }),
        vec![Value::Int(4), Value::Int(3)]
    );
}

#[test]
fn shared_snapshot_remains_isolated_after_a_write() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("value", Value::Int(1))])).unwrap();
    let snapshot = s.snapshot().unwrap();

    s.patch_fields(1, &BTreeMap::from([("value".into(), Value::Int(2))]))
        .unwrap();

    assert_eq!(snapshot.get_field(1, "value").unwrap(), Some(Value::Int(1)));
    assert_eq!(s.get_field(1, "value").unwrap(), Some(Value::Int(2)));
}

#[test]
fn has_value_returns_true_when_any_doc_matches() {
    let mut s = MemoryDocumentStore::new();
    s.put(1, doc([("color", Value::Str("red".into()))]))
        .unwrap();
    s.put(2, doc([("color", Value::Str("blue".into()))]))
        .unwrap();
    assert!(s.has_value("color", &Value::Str("red".into())).unwrap());
    assert!(!s.has_value("color", &Value::Str("green".into())).unwrap());
}

#[test]
fn find_doc_id_by_field_returns_first_match() {
    let mut s = MemoryDocumentStore::new();
    s.put(3, doc([("public_id", Value::Str("m-3".into()))]))
        .unwrap();
    s.put(7, doc([("public_id", Value::Str("m-7".into()))]))
        .unwrap();

    assert_eq!(
        s.find_doc_id_by_field("public_id", &Value::Str("m-7".into()))
            .unwrap(),
        Some(7)
    );
    assert_eq!(
        s.find_doc_id_by_field("public_id", &Value::Str("missing".into()))
            .unwrap(),
        None
    );
}

#[test]
fn patch_fields_updates_and_removes_top_level_values() {
    let mut s = MemoryDocumentStore::new();
    s.put(
        1,
        doc([
            ("public_id", Value::Str("m-1".into())),
            ("content", Value::Str("old".into())),
            ("token_count", Value::Int(4)),
        ]),
    )
    .unwrap();

    let updates = BTreeMap::from([
        ("content".to_string(), Value::Str("new".into())),
        ("token_count".to_string(), Value::Null),
    ]);
    assert!(s.patch_fields(1, &updates).unwrap());

    let got = s.get(1).unwrap().unwrap();
    assert_eq!(got.get("public_id"), Some(&Value::Str("m-1".into())));
    assert_eq!(got.get("content"), Some(&Value::Str("new".into())));
    assert!(!got.contains_key("token_count"));
}

#[test]
fn eval_path_walks_nested_map() {
    let mut s = MemoryDocumentStore::new();
    let mut nested = BTreeMap::new();
    nested.insert("name".to_string(), Value::Str("alice".into()));
    s.put(1, doc([("user", Value::Map(nested))])).unwrap();
    let path = vec![
        uqa_core::PathSegment::Key("user".into()),
        uqa_core::PathSegment::Key("name".into()),
    ];
    assert_eq!(
        s.eval_path(1, &path).unwrap(),
        Some(Value::Str("alice".into()))
    );
}

#[test]
fn iter_all_yields_in_id_order() {
    let mut s = MemoryDocumentStore::new();
    s.put(3, doc([("k", Value::Int(3))])).unwrap();
    s.put(1, doc([("k", Value::Int(1))])).unwrap();
    s.put(2, doc([("k", Value::Int(2))])).unwrap();
    let collected: Vec<u64> = s.iter_all().unwrap().map(|(id, _)| id).collect();
    assert_eq!(collected, vec![1, 2, 3]);
}
