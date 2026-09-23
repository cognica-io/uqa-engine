//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn defaulted_view() -> (StorageReadControl, MaterializedTable) {
    let source_columns = columns("CREATE TABLE t (a INTEGER)");
    let mut target = source_columns.clone();
    let additions = columns("CREATE TABLE t (first TEXT, second TEXT, computed INTEGER GENERATED ALWAYS AS (a + 1) VIRTUAL)");
    for (ordinal, mut column) in additions.into_iter().enumerate() {
        column.object_id = Some([u8::try_from(ordinal + 8).unwrap(); 16]);
        if column.generated.is_none() {
            column.missing_value = Some(Value::Str("x".repeat(4096)));
        }
        target.push(column);
    }
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(1, document(&[("a", Value::Int(10))], 41))
        .unwrap();
    source
        .put_stored(
            3,
            document(
                &[
                    ("a", Value::Int(30)),
                    ("first", Value::Null),
                    ("second", Value::Null),
                ],
                43,
            ),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 10);
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([(2, Some(document(&[("a", Value::Int(20))], 42)))]),
        &control,
    )
    .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        source.snapshot().unwrap(),
        &source_columns,
        &schema(&target, &index),
        changes,
        &control,
    )
    .unwrap();
    source.clear().unwrap();
    (control, view)
}

#[test]
fn owned_row_defaults_hold_their_combined_allowance_until_completion() {
    let (control, view) = defaulted_view();
    let nested = view.documents.snapshot().unwrap();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained - (6 << 10))
        .unwrap();
    for documents in [view.documents.as_ref(), nested.as_ref()] {
        for id in [1, 2] {
            // Either payload fits alone, but a completed row must hold both copies together.
            let field = documents.get_field(id, "first").unwrap().unwrap();
            assert_eq!(field, Value::Str("x".repeat(4096)));
            let error = documents.get_stored(id).unwrap_err();
            assert_eq!(
                snapshot_error("owned defaults", &error).sqlstate(),
                Some("53200")
            );
            let error = documents.get_stored_many(&[3, id]).unwrap_err();
            assert_eq!(
                snapshot_error("owned defaults", &error).sqlstate(),
                Some("53200")
            );
            assert_eq!(control.memory().used(), retained + occupied.bytes());
        }
        let nulls = documents.get_stored(3).unwrap().unwrap();
        assert_eq!(nulls.fields()["first"], Value::Null);
        assert_eq!(nulls.fields()["second"], Value::Null);
        assert_eq!(nulls.metadata().tuple_xmin(), Some(43));
    }
    drop(occupied);
    for id in [1, 2] {
        let row = nested.get_stored(id).unwrap().unwrap();
        assert_eq!(row.fields()["first"], Value::Str("x".repeat(4096)));
        assert_eq!(row.fields()["second"], Value::Str("x".repeat(4096)));
        assert_eq!(
            row.metadata().tuple_xmin(),
            Some(u32::try_from(id + 40).unwrap())
        );
        assert_eq!(control.memory().used(), retained);
    }
    drop(view);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn owned_row_batches_keep_completed_defaults_charged_until_handoff() {
    let (control, view) = defaulted_view();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained - (12 << 10))
        .unwrap();
    for id in [1, 2] {
        assert!(view.documents.get_stored(id).unwrap().is_some());
    }
    // A base row and a private row fit separately, but their completed copies coexist in a batch.
    let error = view.documents.get_stored_many(&[2, 1, 1, 3]).unwrap_err();
    assert_eq!(
        snapshot_error("default batch", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(control.memory().used(), retained + occupied.bytes());
    drop(occupied);
    let rows = view.documents.get_stored_many(&[2, 1, 1, 3]).unwrap();
    assert_eq!(rows.len(), 3);
    for id in [1, 2] {
        assert_eq!(rows[&id].fields()["first"], Value::Str("x".repeat(4096)));
        assert_eq!(rows[&id].fields()["second"], Value::Str("x".repeat(4096)));
    }
    assert_eq!(control.memory().used(), retained);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generated_projection_keeps_default_copies_charged_through_the_callback() {
    let (control, view) = defaulted_view();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained - (6 << 10))
        .unwrap();
    let fields = ["computed", "first", "second"];
    for id in [1, 2] {
        let error = view
            .documents
            .for_each_fields_multi_ref_with_presence(&[id], &fields, &mut |_, _, _| {
                panic!("partial defaults must not reach the visitor")
            })
            .unwrap_err();
        assert_eq!(
            snapshot_error("default projection", &error).sqlstate(),
            Some("53200")
        );
        assert_eq!(control.memory().used(), retained + occupied.bytes());
    }
    drop(occupied);
    let mut visited = Vec::new();
    view.documents
        .for_each_fields_multi_ref_with_presence(
            &[2, 1, 99, 2],
            &fields,
            &mut |id, present, values| {
                visited.push(id);
                assert_eq!(present, id != 99);
                if present {
                    assert_eq!(values[0], &Value::Int(i64::try_from(id * 10 + 1).unwrap()));
                    assert_eq!(values[1], &Value::Str("x".repeat(4096)));
                    assert_eq!(values[2], values[1]);
                    assert!(control.memory().used() >= retained + (8 << 10));
                }
                true
            },
        )
        .unwrap();
    assert_eq!(visited, [2, 1, 99, 2]);
    assert_eq!(control.memory().used(), retained);
    let error = view
        .documents
        .for_each_fields_multi_ref_with_presence(&[1, 2], &fields, &mut |_, _, _| {
            control.cancellation().cancel();
            false
        })
        .unwrap_err();
    assert_eq!(
        snapshot_error("default projection", &error).sqlstate(),
        Some("57014")
    );
    assert_eq!(control.memory().used(), retained);
    control.cancellation().reset();
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn missing_default_payloads_reject_before_copy_and_preserve_nulls_and_absent_rows() {
    let (control, view) = defaulted_view();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    for id in [1, 2] {
        let error = view.documents.get_field(id, "first").unwrap_err();
        assert_eq!(
            snapshot_error("owned default", &error).sqlstate(),
            Some("53200")
        );
    }
    assert_eq!(
        view.documents.get_field(3, "first").unwrap(),
        Some(Value::Null)
    );
    assert_eq!(view.documents.get_field(99, "first").unwrap(), None);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(occupied);
    control.cancellation().cancel();
    let error = view.documents.get_field(1, "first").unwrap_err();
    assert_eq!(
        snapshot_error("owned default", &error).sqlstate(),
        Some("57014")
    );
    control.cancellation().reset();
    assert_eq!(
        view.documents
            .get_stored(1)
            .unwrap()
            .unwrap()
            .metadata()
            .tuple_xmin(),
        Some(41)
    );
    assert_eq!(control.memory().used(), retained);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unchanged_owned_column_names_need_no_mapping_allocation() {
    let columns = columns("CREATE TABLE t (a INTEGER, b INTEGER)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut source = MemoryDocumentStore::new();
    let original = document(&[("a", Value::Int(10)), ("b", Value::Null)], 41);
    source.put_stored(1, original.clone()).unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let view = retain(
        source.snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert_eq!(view.documents.get_stored(1).unwrap(), Some(original));
    drop(occupied);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
