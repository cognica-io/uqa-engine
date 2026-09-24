//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn selected_column_payloads_are_admitted_once_and_survive_nested_readers() {
    let mut columns = columns("CREATE TABLE t (body TEXT)");
    columns[0].missing_value = Some(Value::Str("selected".repeat(4096)));
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let schema = schema(&columns, &index);
    let definitions = Arc::downgrade(&schema.columns);
    let source: Arc<dyn DocumentStore> = Arc::new(MemoryDocumentStore::new());
    let rejected = StorageReadControl::with_limit(4096);
    let error = retain(
        Arc::clone(&source),
        &columns,
        &schema,
        DocumentChanges::default(),
        &rejected,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(rejected.memory().used(), 0);

    let control = StorageReadControl::with_limit(128 << 10);
    let admitted =
        RetainedColumns::capture(&schema.columns, control.memory(), control.cancellation())
            .unwrap();
    let catalog_bytes = admitted.reserved_bytes();
    drop(admitted);
    let view = retain(
        source,
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let retained = control.memory().used();
    assert!(retained >= catalog_bytes);
    assert!(retained < 2 * catalog_bytes);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
    assert_eq!(control.memory().used(), control.memory().limit());
    drop((full, view, schema));
    drop(columns);
    assert!(definitions.upgrade().is_some());
    assert!(control.memory().used() >= catalog_bytes);
    drop(nested);
    assert!(definitions.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}
