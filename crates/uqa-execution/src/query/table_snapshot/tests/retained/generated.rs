//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn generated_view(indexed: bool) -> (StorageReadControl, MaterializedTable, Arc<AtomicBool>) {
    let source_columns = columns("CREATE TABLE t (a INTEGER, opaque TEXT)");
    let mut target = source_columns.clone();
    target[0].name = "renamed".into();
    let additions = columns("CREATE TABLE t (added INTEGER, unused_default TEXT, computed INTEGER GENERATED ALWAYS AS (renamed + added) VIRTUAL, body TEXT GENERATED ALWAYS AS (CAST(renamed + added AS TEXT)) VIRTUAL, bad INTEGER GENERATED ALWAYS AS (1 / 0) VIRTUAL)");
    for (ordinal, mut column) in additions.into_iter().enumerate() {
        column.object_id = Some([u8::try_from(ordinal + 10).unwrap(); 16]);
        if column.name == "added" {
            column.missing_value = Some(Value::Int(17));
        } else if column.name == "unused_default" {
            column.missing_value = Some(Value::Str("unused".repeat(32 << 10)));
        }
        target.push(column);
    }
    let mut source = MemoryDocumentStore::new();
    for id in 1..=3 {
        source
            .put_stored(
                id,
                document(
                    &[
                        ("a", Value::Int(10)),
                        ("opaque", Value::Str("opaque".repeat(32 << 10))),
                    ],
                    41,
                ),
            )
            .unwrap();
    }
    let mut private = MemoryDocumentStore::new();
    private
        .put_stored(
            2,
            document(
                &[
                    ("renamed", Value::Int(30)),
                    ("opaque", Value::Str("private".repeat(32 << 10))),
                ],
                42,
            ),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let cancel_projection = Arc::new(AtomicBool::new(false));
    let retained_source = |rows: &dyn DocumentStore| {
        let mut source = ProjectedSource::new(rows);
        source.cancel_after_projection = Some((
            control.cancellation().clone(),
            Arc::clone(&cancel_projection),
        ));
        Arc::new(source)
    };
    let changes = DocumentChanges::default()
        .with_retained(
            retained_source(&private),
            selection([(2, true), (3, false)]),
            &control,
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let text_fields = if indexed {
        vec!["body".into()]
    } else {
        Vec::new()
    };
    let mut selected = schema(&target, &index);
    selected.text_fields = &text_fields;
    let view = retain(
        retained_source(&source),
        &source_columns,
        &selected,
        changes,
        &control,
    )
    .unwrap();
    source.clear().unwrap();
    private.clear().unwrap();
    (control, view, cancel_projection)
}

#[test]
fn generated_reads_borrow_selected_base_private_and_default_inputs() {
    let (control, view, _) = generated_view(false);
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    for documents in [view.documents.as_ref(), nested.as_ref()] {
        assert_eq!(
            documents.get_field(1, "computed").unwrap(),
            Some(Value::Int(27))
        );
        assert_eq!(
            documents.get_field(2, "computed").unwrap(),
            Some(Value::Int(47))
        );
        assert_eq!(documents.get_field(3, "computed").unwrap(), None);
        assert_eq!(documents.get_field(99, "computed").unwrap(), None);
        assert_eq!(
            documents.get_metadata(2).unwrap().unwrap().tuple_xmin(),
            Some(42)
        );
        assert_eq!(
            documents
                .get_fields_multi(&[2, 1, 2, 3, 99], &["computed", "renamed"])
                .unwrap(),
            BTreeMap::from([
                (1, vec![Value::Int(27), Value::Int(10)]),
                (2, vec![Value::Int(47), Value::Int(30)]),
            ])
        );
        let error = documents.get_field(1, "bad").unwrap_err();
        assert_eq!(
            snapshot_error("generated read", &error).sqlstate(),
            Some("22012")
        );
    }
    let retained = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    let error = nested.get_field(1, "computed").unwrap_err();
    assert_eq!(
        snapshot_error("generated read", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(nested.get_field(99, "computed").unwrap(), None);
    drop(full);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        nested.get_field(1, "computed").unwrap(),
        Some(Value::Int(27))
    );
    control.cancellation().cancel();
    let error = nested.get_field(1, "computed").unwrap_err();
    assert_eq!(
        snapshot_error("generated read", &error).sqlstate(),
        Some("57014")
    );
    control.cancellation().reset();
    drop(view);
    assert!(control.memory().used() > 0);
    assert_eq!(
        nested.get_field(2, "computed").unwrap(),
        Some(Value::Int(47))
    );
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generated_index_capture_keeps_opaque_rows_and_unrequested_defaults_borrowed() {
    let (control, view, _) = generated_view(true);
    for (term, id) in [("27", 1), ("47", 2)] {
        assert_eq!(
            view.text
                .get_posting_list("body", term)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [id]
        );
    }
    assert_eq!(view.text.field_doc_count("body").unwrap(), 2);
    assert_eq!(
        view.documents.get_field(1, "computed").unwrap(),
        Some(Value::Int(27))
    );
    drop(view);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn generated_reads_keep_evaluator_errors_before_provider_return_cancellation() {
    let (control, view, cancel_projection) = generated_view(false);
    let retained = control.memory().used();
    for (id, value) in [(1, 27), (2, 47)] {
        for (field, expected) in [("computed", "57014"), ("bad", "22012")] {
            cancel_projection.store(true, Ordering::Relaxed);
            let error = view.documents.get_field(id, field).unwrap_err();
            assert_eq!(
                snapshot_error("generated read", &error).sqlstate(),
                Some(expected)
            );
            assert!(control.cancellation().is_cancelled());
            assert_eq!(control.memory().used(), retained);
            control.cancellation().reset();
        }
        assert_eq!(
            view.documents.get_field(id, "computed").unwrap(),
            Some(Value::Int(value))
        );
    }
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
