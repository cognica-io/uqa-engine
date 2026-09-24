//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn generated_source() -> MemoryDocumentStore {
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(1, document(&[("a", Value::Int(0))], 41))
        .unwrap();
    source
        .put_stored(2, document(&[("a", Value::Int(10))], 42))
        .unwrap();
    source
}

fn private_zero(control: &StorageReadControl) -> DocumentChanges {
    DocumentChanges::from_rows(
        BTreeMap::from([(2, Some(document(&[("a", Value::Int(0))], 43)))]),
        control,
    )
    .unwrap()
}

#[test]
fn captured_rows_evaluate_only_the_requested_generated_columns() {
    let columns = columns("CREATE TABLE t (a INTEGER, bad INTEGER GENERATED ALWAYS AS (12/a) VIRTUAL, good INTEGER GENERATED ALWAYS AS (a+1) VIRTUAL)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &text);
    for copied in [false, true] {
        let control = control();
        let mut source = generated_source();
        let changes = private_zero(&control);
        let view = if copied {
            materialize(&source, &columns, &schema, changes, &control)
        } else {
            retain(
                source.snapshot().unwrap(),
                &columns,
                &schema,
                changes,
                &control,
            )
        }
        .unwrap();
        source.clear().unwrap();
        assert_eq!(view.document_count, 2);
        assert_eq!(view.documents.doc_ids().unwrap(), [1, 2]);
        for (id, xmin) in [(1, 41), (2, 43)] {
            assert_eq!(
                view.documents.get_metadata(id).unwrap(),
                Some(DocumentMetadata::with_tuple_xmin(xmin))
            );
            assert_eq!(
                view.documents.get_field(id, "a").unwrap(),
                Some(Value::Int(0))
            );
            assert_eq!(
                view.documents.get_field(id, "good").unwrap(),
                Some(Value::Int(1))
            );
            for error in [
                view.documents.get_field(id, "bad").unwrap_err(),
                view.documents.get_stored(id).unwrap_err(),
            ] {
                assert_eq!(
                    crate::storage_errors::storage_error("read generated field", &error).sqlstate(),
                    Some("22012")
                );
            }
            assert_eq!(
                view.documents.get_field(id, "good").unwrap(),
                Some(Value::Int(1))
            );
        }
        assert_eq!(
            view.documents
                .get_fields_multi(&[2, 1, 2, 99], &["good", "a"])
                .unwrap(),
            BTreeMap::from([
                (1, vec![Value::Int(1), Value::Int(0)]),
                (2, vec![Value::Int(1), Value::Int(0)]),
            ])
        );
        drop(view);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn captured_indexes_do_not_evaluate_unrelated_generated_siblings() {
    let columns = columns("CREATE TABLE t (a INTEGER, bad INTEGER GENERATED ALWAYS AS (12/a) VIRTUAL, body TEXT GENERATED ALWAYS AS ('safe') VIRTUAL)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let fields = ["body".into()];
    let mut schema = schema(&columns, &text);
    schema.text_fields = &fields;
    for copied in [false, true] {
        let control = control();
        let source = generated_source();
        let changes = private_zero(&control);
        let view = if copied {
            materialize(&source, &columns, &schema, changes, &control)
        } else {
            retain(
                source.snapshot().unwrap(),
                &columns,
                &schema,
                changes,
                &control,
            )
        }
        .unwrap();
        drop(source);
        assert_eq!(
            view.text
                .get_posting_list("body", "safe")
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(view.text.field_doc_count("body").unwrap(), 2);
        assert_eq!(
            view.documents.get_field(2, "body").unwrap(),
            Some(Value::Str("safe".into()))
        );
        drop(view);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn nested_copied_captures_keep_immutable_inputs_without_whole_row_evaluation() {
    let columns = columns("CREATE TABLE t (a INTEGER, bad INTEGER GENERATED ALWAYS AS (12/a) VIRTUAL, good INTEGER GENERATED ALWAYS AS (a+1) VIRTUAL)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &text);
    let owner = control();
    let caller = control();
    let source = generated_source();
    let original = materialize(&source, &columns, &schema, private_zero(&owner), &owner).unwrap();
    let nested = materialize(
        original.documents.as_ref(),
        &columns,
        &schema,
        DocumentChanges::default(),
        &caller,
    )
    .unwrap();
    drop(source);
    drop(original);
    assert!(owner.memory().used() > 0);
    assert_eq!(nested.documents.doc_ids().unwrap(), [1, 2]);
    assert_eq!(
        nested.documents.get_field(2, "good").unwrap(),
        Some(Value::Int(1))
    );
    assert_eq!(
        nested
            .documents
            .get_metadata(2)
            .unwrap()
            .unwrap()
            .tuple_xmin(),
        Some(43)
    );
    owner.cancellation().cancel();
    assert!(nested.documents.get_field(1, "good").is_err());
    caller.cancellation().cancel();
    let rejected = materialize(
        nested.documents.as_ref(),
        &columns,
        &schema,
        DocumentChanges::default(),
        &control(),
    );
    assert_eq!(rejected.err().unwrap().sqlstate(), Some("57014"));
    drop(nested);
    assert_eq!(owner.memory().used(), 0);
    assert_eq!(caller.memory().used(), 0);
}
