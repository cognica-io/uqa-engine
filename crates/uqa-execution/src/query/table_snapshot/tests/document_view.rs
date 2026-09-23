//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod cancellation;

fn capture(control: &StorageReadControl) -> MaterializedTable {
    let mut source = MemoryDocumentStore::new();
    for id in 1..=3 {
        source
            .put_stored(id, document(&[("key", Value::Int(10))], 41))
            .unwrap();
    }
    let columns = columns("CREATE TABLE t (key INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    retain(
        source.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::from_rows(
            BTreeMap::from([
                (2, None),
                (3, Some(document(&[("key", Value::Int(30))], 42))),
            ]),
            control,
        )
        .unwrap(),
        control,
    )
    .unwrap()
}

#[test]
fn document_view_workspace_rejects_exhaustion_without_changing_retained_rows() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let view = capture(&control);
    let retained = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    let mut visited = false;
    let projection = view.documents.for_each_fields_multi_ref_with_presence(
        &[1, 3],
        &["key"],
        &mut |_, _, _| {
            visited = true;
            true
        },
    );
    for (name, result) in [
        (
            "identity merge",
            view.documents.next_doc_ids(None, 3).map(|_| ()),
        ),
        ("projection", projection),
        (
            "owned selection",
            view.documents.get_stored_many(&[1, 3]).map(|_| ()),
        ),
    ] {
        assert!(
            matches!(result, Err(StorageBackendError::Memory(_))),
            "{name}: {result:?}"
        );
    }
    assert!(!visited);
    assert!(view.documents.next_doc_ids(None, 0).unwrap().is_empty());
    view.documents
        .for_each_fields_multi_ref_with_presence(&[], &["key"], &mut |_, _, _| {
            panic!("an empty request must not visit rows")
        })
        .unwrap();
    drop(full);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(view.documents.next_doc_ids(None, 3).unwrap(), [1, 3]);
    let mut actual = Vec::new();
    view.documents
        .for_each_fields_multi_ref_with_presence(
            &[3, 1, 2, 3],
            &["key", "key"],
            &mut |id, present, values| {
                actual.push((
                    id,
                    present,
                    values.iter().map(|v| (*v).clone()).collect::<Vec<_>>(),
                ));
                true
            },
        )
        .unwrap();
    assert_eq!(
        actual,
        [
            (3, true, vec![Value::Int(30); 2]),
            (1, true, vec![Value::Int(10); 2]),
            (2, false, vec![Value::Null; 2]),
            (3, true, vec![Value::Int(30); 2]),
        ]
    );
    assert_eq!(control.memory().used(), retained);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn document_view_workspace_stays_charged_while_its_consumer_runs() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let view = capture(&control);
    let retained = control.memory().used();
    let mut visited = 0;
    assert_eq!(view.documents.for_each_next_fields(None, 3, &["key"], &mut |_, _| {
        assert!(control.memory().used() > retained, "identity and projection buffers must retain their allowance through the callback");
        visited += 1;
        true
    }).unwrap(), Some(2));
    assert_eq!(visited, 2);
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn nested_document_views_keep_the_original_cancellation_for_every_read_shape() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let view = capture(&control);
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    control.cancellation().cancel();
    for documents in [view.documents.as_ref(), nested.as_ref()] {
        for id in [1, 2, 3, 99] {
            for (name, result) in [
                ("document", documents.get_stored(id).map(|_| ())),
                ("field", documents.get_field(id, "key").map(|_| ())),
                ("metadata", documents.get_metadata(id).map(|_| ())),
                ("presence", documents.contains_doc_id(id).map(|_| ())),
            ] {
                assert!(
                    matches!(result, Err(StorageBackendError::Cancelled(_))),
                    "{name}, {id}: {result:?}"
                );
            }
        }
        for (name, result) in [
            ("count", documents.len().map(|_| ())),
            ("snapshot", documents.snapshot().map(|_| ())),
            (
                "empty composite lookup",
                documents.find_doc_id_by_fields(&[], &[]).map(|_| ()),
            ),
            (
                "empty projection",
                documents.for_each_fields_multi_ref_with_presence(&[], &[], &mut |_, _, _| {
                    panic!("cancelled")
                }),
            ),
        ] {
            assert!(
                matches!(result, Err(StorageBackendError::Cancelled(_))),
                "{name}: {result:?}"
            );
        }
    }
    drop(view);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn wide_document_layout_is_admitted_before_publication_and_preserves_a_prior_view() {
    let control = StorageReadControl::with_limit(16 * 1024);
    let prior = capture(&control);
    let retained = control.memory().used();
    let template = columns("CREATE TABLE t (key INTEGER)").remove(0);
    let columns = (1_u128..=1000)
        .map(|id| {
            let mut column = template.clone();
            column.name = format!("column_{id:04}");
            column.object_id = Some(id.to_be_bytes());
            column
        })
        .collect::<Vec<_>>();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let error = retain(
        Arc::new(MemoryDocumentStore::new()),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::default(),
        &control,
    )
    .err()
    .expect("column mapping must share the query allowance");
    assert_eq!(error.sqlstate(), Some("53200"), "{error}");
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        prior.documents.get_field(3, "key").unwrap(),
        Some(Value::Int(30))
    );
    drop(prior);
    assert_eq!(control.memory().used(), 0);
}
