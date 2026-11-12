//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};
use uqa_core::{
    memory::{Budgeted, BudgetedString},
    Value,
};
use uqa_storage::{
    inverted_index::AnalyzerDefault, read_control::StorageReadControl, KeyValueStorageBackend,
    PersistentStorageBackend, StorageName,
};

mod vectors;

#[test]
fn redb_controlled_handles_preserve_writes_rollback_and_reopen_after_input_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("handles.redb");
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    let control = StorageReadControl::with_limit(1 << 20);
    let mut name = BudgetedString::new(control.memory());
    name.push_str("public.notes").unwrap();
    let (name, memory) = name.into_parts();
    let name = StorageName::from_admitted(Budgeted::new(name, memory)).unwrap();
    let bytes = serde_json::to_vec(&uqa_analysis::whitespace_analyzer()).unwrap();
    let memory = control.memory().reserve(bytes.capacity()).unwrap();
    let definition =
        AnalyzerDefinition::from_image(Budgeted::new(bytes, memory), control.cancellation())
            .unwrap();
    let default =
        AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), &control)
            .unwrap();
    let mut docs = backend
        .document_store_with_control(name.clone(), &control)
        .unwrap();
    let mut index = backend
        .inverted_index_with_control(name, default, &control)
        .unwrap();
    control.cancellation().cancel();
    docs.put(1, [("body".into(), Value::Str("First WORD".into()))].into())
        .unwrap();
    index
        .add_document(1, [("body".into(), "First WORD".into())].into())
        .unwrap();
    backend.begin_transaction().unwrap();
    docs.put(2, [("body".into(), Value::Str("discarded".into()))].into())
        .unwrap();
    index
        .add_document(2, [("body".into(), "discarded".into())].into())
        .unwrap();
    backend.rollback_transaction().unwrap();
    assert_eq!(docs.len().unwrap(), 1);
    assert_eq!(index.doc_freq("body", "discarded").unwrap(), 0);
    drop((docs, index, backend, storage));
    assert_eq!(control.memory().used(), 0);
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    let docs = backend.document_store("public.notes");
    let index = backend.inverted_index("public.notes", uqa_analysis::whitespace_analyzer());
    assert_eq!(
        docs.get_field(1, "body").unwrap(),
        Some(Value::Str("First WORD".into()))
    );
    assert_eq!(docs.len().unwrap(), 1);
    assert_eq!(index.doc_freq("body", "word").unwrap(), 1);
    assert_eq!(index.doc_freq("body", "discarded").unwrap(), 0);
}
