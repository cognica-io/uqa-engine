//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn private_source_uses_current_column_layout_and_survives_source_mutation() {
    let (columns, target, mut base) = renamed_documents();
    let mut private = MemoryDocumentStore::new();
    private
        .put_stored(
            3,
            document(
                &[
                    ("renamed", Value::Int(300)),
                    ("opaque", Value::Str("private".repeat(1024))),
                ],
                52,
            ),
        )
        .unwrap();
    private
        .put_stored(
            7,
            document(&[("renamed", Value::Int(700)), ("added", Value::Null)], 53),
        )
        .unwrap();
    let private_probe = ProjectedSource::new(&private);
    let projections = Arc::clone(&private_probe.projections);
    let changes = DocumentChanges::default()
        .with_retained(
            Arc::new(private_probe),
            selection([(3, true), (5, false), (7, true)]),
            &control(),
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        Arc::new(ProjectedSource::new(&base)),
        &columns,
        &schema(&target, &index),
        changes,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(projections.load(Ordering::Relaxed), 0);
    assert_eq!(view.document_count, 4);
    private.clear().unwrap();
    base.clear().unwrap();
    let nested = view.documents.snapshot().unwrap();
    drop(view);
    let values = nested
        .get_fields_multi(&[3, 1, 7, 5, 3], &["renamed", "added", "a"])
        .unwrap();
    assert_eq!(
        values,
        [
            (1, vec![Value::Int(10), Value::Int(17), Value::Null]),
            (3, vec![Value::Int(300), Value::Int(17), Value::Null]),
            (7, vec![Value::Int(700), Value::Null, Value::Null]),
        ]
        .into()
    );
    assert_eq!(
        nested.get_metadata(3).unwrap().unwrap().tuple_xmin(),
        Some(52)
    );
    assert_eq!(
        nested.next_doc_ids(Some(1), 3).unwrap(),
        vec![3, 7, u64::MAX]
    );
    assert!(projections.load(Ordering::Relaxed) > 0);
}

#[test]
fn stored_generated_private_fields_need_no_unrelated_payload_reads() {
    let columns = columns(
        "CREATE TABLE t (a INT, generated INT GENERATED ALWAYS AS (a + 1) STORED, opaque TEXT)",
    );
    let mut private = MemoryDocumentStore::new();
    private
        .put_stored(
            2,
            document(
                &[
                    ("a", Value::Int(20)),
                    ("generated", Value::Int(21)),
                    ("opaque", Value::Str("payload".repeat(1024))),
                ],
                51,
            ),
        )
        .unwrap();
    let changes = DocumentChanges::default()
        .with_retained(
            Arc::new(ProjectedSource::new(&private)),
            selection([(2, true)]),
            &control(),
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        MemoryDocumentStore::new().snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        changes,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        view.documents.get_field(2, "generated").unwrap(),
        Some(Value::Int(21))
    );
    assert_eq!(
        view.documents
            .get_fields_multi(&[2], &["generated"])
            .unwrap(),
        [(2, vec![Value::Int(21)])].into()
    );
}

#[test]
fn missing_generated_private_values_are_completed_from_the_selected_source() {
    let columns =
        columns("CREATE TABLE t (a INT, generated INT GENERATED ALWAYS AS (a + 1) VIRTUAL)");
    let mut private = MemoryDocumentStore::new();
    private
        .put_stored(2, document(&[("a", Value::Int(20))], 51))
        .unwrap();
    let changes = DocumentChanges::default()
        .with_retained(
            private.snapshot().unwrap(),
            selection([(2, true)]),
            &control(),
        )
        .unwrap();
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let view = retain(
        MemoryDocumentStore::new().snapshot().unwrap(),
        &columns,
        &schema(&columns, &index),
        changes,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        view.documents
            .get_fields_multi(&[2], &["generated"])
            .unwrap(),
        [(2, vec![Value::Int(21)])].into()
    );
}
