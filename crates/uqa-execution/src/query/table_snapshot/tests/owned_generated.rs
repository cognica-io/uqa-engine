//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn generated_view() -> (StorageReadControl, MaterializedTable) {
    let source_columns = columns("CREATE TABLE t (a TEXT)");
    let target = columns("CREATE TABLE t (a TEXT, first TEXT GENERATED ALWAYS AS (repeat(a, 32768)) VIRTUAL, second TEXT GENERATED ALWAYS AS (repeat(a, 32768)) VIRTUAL)");
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(1, document(&[("a", Value::Str("x".into()))], 41))
        .unwrap();
    source
        .put_stored(
            3,
            document(
                &[
                    ("a", Value::Str("z".into())),
                    ("first", Value::Null),
                    ("second", Value::Null),
                ],
                43,
            ),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([(2, Some(document(&[("a", Value::Str("y".into()))], 42)))]),
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
    (control, view)
}

#[test]
fn generated_field_projection_keeps_both_outputs_charged_through_the_callback() {
    let (control, view) = generated_view();
    let nested = view.documents.snapshot().unwrap();
    let retained = control.memory().used();
    let mut visits = Vec::new();
    nested
        .for_each_fields_multi_ref_with_presence(
            &[2, 1, 99, 2],
            &["first", "second"],
            &mut |id, present, values| {
                visits.push(id);
                assert_eq!(present, id != 99);
                if present {
                    let text = if id == 1 { "x" } else { "y" };
                    assert_eq!(values[0], &Value::Str(text.repeat(32768)));
                    assert_eq!(values[1], values[0]);
                    assert!(control.memory().used() >= retained + 65536);
                } else {
                    assert_eq!(values, &[&Value::Null, &Value::Null]);
                }
                true
            },
        )
        .unwrap();
    assert_eq!(visits, [2, 1, 99, 2]);
    assert_eq!(control.memory().used(), retained);
    drop(view);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generated_field_quota_failure_releases_partial_work_and_can_read_after_release() {
    let (control, view) = generated_view();
    let retained = control.memory().used();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - retained - 8192)
        .unwrap();
    for id in [1, 2] {
        let error = view.documents.get_field(id, "first").unwrap_err();
        assert_eq!(
            snapshot_error("generated output", &error).sqlstate(),
            Some("53200")
        );
        assert_eq!(control.memory().used(), retained + occupied.bytes());
    }
    drop(occupied);
    for (id, text) in [(1, "x"), (2, "y")] {
        assert_eq!(
            view.documents.get_field(id, "first").unwrap(),
            Some(Value::Str(text.repeat(32768)))
        );
        assert_eq!(control.memory().used(), retained);
    }
    assert_eq!(view.documents.get_field(99, "first").unwrap(), None);
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generated_whole_rows_preserve_metadata_existing_nulls_and_batch_ownership() {
    let (control, view) = generated_view();
    let retained = control.memory().used();
    let rows = view.documents.get_stored_many(&[2, 1, 1, 3, 99]).unwrap();
    assert_eq!(rows.len(), 3);
    for (id, text) in [(1, "x"), (2, "y")] {
        let row = &rows[&id];
        assert_eq!(row.fields()["first"], Value::Str(text.repeat(32768)));
        assert_eq!(row.fields()["second"], row.fields()["first"]);
        assert_eq!(
            row.metadata().tuple_xmin(),
            Some(u32::try_from(id + 40).unwrap())
        );
    }
    assert_eq!(rows[&3].fields()["first"], Value::Null);
    assert_eq!(rows[&3].fields()["second"], Value::Null);
    assert_eq!(rows[&3].metadata().tuple_xmin(), Some(43));
    assert_eq!(control.memory().used(), retained);
    let error = view
        .documents
        .for_each_fields_multi_ref_with_presence(&[1, 2], &["first", "second"], &mut |_, _, _| {
            assert!(control.memory().used() >= retained + 65536);
            control.cancellation().cancel();
            false
        })
        .unwrap_err();
    assert_eq!(
        snapshot_error("generated cancellation", &error).sqlstate(),
        Some("57014")
    );
    assert_eq!(control.memory().used(), retained);
    control.cancellation().reset();
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
