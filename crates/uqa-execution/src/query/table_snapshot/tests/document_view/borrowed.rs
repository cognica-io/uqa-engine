//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn borrowed_query_projection_preserves_nulls_duplicates_and_order() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut source = MemoryDocumentStore::new();
    source
        .put(1, BTreeMap::from([("key".into(), Value::Int(11))]))
        .unwrap();
    source
        .put(3, BTreeMap::from([("key".into(), Value::Null)]))
        .unwrap();
    source.put(5, BTreeMap::new()).unwrap();
    let columns = columns("CREATE TABLE t (key INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        source.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let retained = control.memory().used();
    let mut actual = Vec::new();
    assert_eq!(
        view.documents
            .for_each_next_fields_borrowed(Some(1), 2, &["key", "key"], &mut |id, values| {
                actual.push((
                    id,
                    values
                        .iter()
                        .map(|value| (*value).clone())
                        .collect::<Vec<_>>(),
                ));
                true
            })
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        actual,
        [(3, vec![Value::Null; 2]), (5, vec![Value::Null; 2])]
    );
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn borrowed_query_projection_does_not_advance_private_row_fallbacks() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let view = capture(&control);
    assert_eq!(
        view.documents
            .for_each_next_fields_borrowed(None, 3, &["key"], &mut |_, _| panic!(
                "private rows must use the retained merge"
            ))
            .unwrap(),
        None
    );
    assert_eq!(view.documents.next_doc_ids(None, 3).unwrap(), [1, 3]);
    assert_eq!(
        view.documents
            .for_each_fields_multi_borrowed(&[1, 3], &["key"], &mut |_, _, _| panic!(
                "private rows require the retained merge"
            ),)
            .unwrap(),
        None
    );
}

#[test]
fn borrowed_query_points_preserve_missing_rows_and_duplicate_positions() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut source = MemoryDocumentStore::new();
    source
        .put(1, BTreeMap::from([("key".into(), Value::Int(11))]))
        .unwrap();
    source
        .put(3, BTreeMap::from([("key".into(), Value::Null)]))
        .unwrap();
    let columns = columns("CREATE TABLE t (key INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        source.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let retained = control.memory().used();
    let mut actual = Vec::new();
    assert_eq!(
        view.documents
            .for_each_fields_multi_borrowed(
                &[3, 9, 1, 1],
                &["key", "key"],
                &mut |id, present, values| {
                    assert_eq!(values[0], values[1]);
                    actual.push((id, present, values[0].clone()));
                    true
                },
            )
            .unwrap(),
        Some(4)
    );
    assert_eq!(
        actual,
        [
            (3, true, Value::Null),
            (9, false, Value::Null),
            (1, true, Value::Int(11)),
            (1, true, Value::Int(11)),
        ]
    );
    assert_eq!(control.memory().used(), retained);
    let mut count = 0;
    assert_eq!(
        view.documents
            .for_each_fields_multi_borrowed(&[1, 3], &["key"], &mut |_, _, _| {
                count += 1;
                false
            },)
            .unwrap(),
        Some(1)
    );
    assert_eq!(count, 1);
}
