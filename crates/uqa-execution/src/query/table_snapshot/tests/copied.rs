//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod generated;

#[test]
fn copied_unindexed_rows_retain_their_allowance_after_nested_capture_and_failed_replacement() {
    let columns = columns("CREATE TABLE t (body TEXT)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &text);
    let control = StorageReadControl::with_limit(128 * 1024);
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            document(&[("body", Value::Str("first".repeat(4096)))], 41),
        )
        .unwrap();
    source
        .put_stored(
            2,
            document(&[("body", Value::Str("second".repeat(4096)))], 42),
        )
        .unwrap();
    let capture = |source: &MemoryDocumentStore| {
        materialize(
            source,
            &columns,
            &schema,
            DocumentChanges::default(),
            &control,
        )
    };
    let view = capture(&source).unwrap();
    let retained = control.memory().used();
    assert!(retained >= 11 * 4096);
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    assert_eq!(control.memory().used(), retained);
    source
        .put_stored(
            3,
            document(&[("body", Value::Str("oversized".repeat(32768)))], 43),
        )
        .unwrap();
    let error = capture(&source).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(control.memory().used(), retained);
    assert_eq!(view.document_count, 2);
    source.clear().unwrap();
    drop(view);
    assert!(control.memory().used() >= 11 * 4096);
    assert_eq!(
        nested.get_metadata(1).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(41))
    );
    assert_eq!(
        nested.get_field(2, "body").unwrap(),
        Some(Value::Str("second".repeat(4096)))
    );
    assert_eq!(nested.doc_ids().unwrap(), [1, 2]);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn copied_view_shares_selected_defaults_and_preserves_private_tuple_metadata() {
    let source_columns = columns("CREATE TABLE t (old INTEGER)");
    let mut target = source_columns.clone();
    target[0].name = "id".into();
    let mut added = columns("CREATE TABLE x (body TEXT)").remove(0);
    added.object_id = Some([9; 16]);
    added.missing_value = Some(Value::Str("default".repeat(2048)));
    target.push(added);
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&target, &text);
    let definitions = Arc::downgrade(&schema.columns);
    let control = StorageReadControl::with_limit(128 * 1024);
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(7, document(&[("old", Value::Int(70))], 41))
        .unwrap();
    source
        .put_stored(9, document(&[("old", Value::Int(90))], 42))
        .unwrap();
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([
            (2, Some(document(&[("id", Value::Int(20))], 43))),
            (9, None),
        ]),
        &control,
    )
    .unwrap();
    let view = materialize(&source, &source_columns, &schema, changes, &control).unwrap();
    drop(schema);
    drop(target);
    assert_eq!(view.documents.doc_ids().unwrap(), [2, 7]);
    // The selected definition already owns the default; capture keeps that owner instead of producing another default for every row.
    assert!(definitions.upgrade().is_some());
    assert!(control.memory().used() < 7 * 2048);
    for (id, value, xmin) in [(2, 20, 43), (7, 70, 41)] {
        assert_eq!(
            view.documents.get_field(id, "id").unwrap(),
            Some(Value::Int(value))
        );
        assert_eq!(view.documents.get_field(id, "old").unwrap(), None);
        assert_eq!(
            view.documents.get_field(id, "body").unwrap(),
            Some(Value::Str("default".repeat(2048)))
        );
        assert_eq!(
            view.documents.get_metadata(id).unwrap(),
            Some(DocumentMetadata::with_tuple_xmin(xmin))
        );
    }
    drop(view);
    assert!(definitions.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn copied_view_cancellation_preserves_the_original_allowance_until_readers_drop() {
    let columns = columns("CREATE TABLE t (id INTEGER)");
    let text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &text);
    let control = control();
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(1, document(&[("id", Value::Int(1))], 41))
        .unwrap();
    let view = materialize(
        &source,
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let retained = control.memory().used();
    control.cancellation().cancel();
    let error = materialize(
        &source,
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(control.memory().used(), retained);
    assert!(matches!(
        view.documents.get(1),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
