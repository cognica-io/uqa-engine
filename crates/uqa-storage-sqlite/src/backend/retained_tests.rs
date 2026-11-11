//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};
use uqa_core::memory::{Budgeted, BudgetedString};
use uqa_storage::{inverted_index::AnalyzerDefault, read_control::StorageReadControl, StorageName};

fn backend(mode: &str) -> Arc<dyn PersistentStorageBackend> {
    match mode {
        "native" => {
            SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap())
                .open_session()
                .unwrap()
                .backend
        }
        "kv" => Arc::new(uqa_storage::KeyValueStorageBackend::new(Arc::new(
            crate::SQLiteKeyValueStore::open_in_memory().unwrap(),
        ))),
        "legacy" => {
            let connection = ManagedConnection::open_in_memory().unwrap();
            Catalog::open(connection.clone()).unwrap();
            Arc::new(SQLiteStorageBackend::new(connection))
        }
        _ => unreachable!(),
    }
}

fn input(control: &StorageReadControl) -> (StorageName, AnalyzerDefault) {
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
        AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), control)
            .unwrap();
    (name, default)
}

#[test]
fn sqlite_controlled_handles_preserve_mutation_rollback_and_input_independence() {
    for mode in ["native", "kv", "legacy"] {
        let backend = backend(mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let (name, default) = input(&control);
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
        assert_eq!(
            docs.get_field(1, "body").unwrap(),
            Some(Value::Str("First WORD".into())),
            "{mode}"
        );
        assert_eq!(index.doc_freq("body", "word").unwrap(), 1, "{mode}");
        backend.begin_transaction().unwrap();
        docs.put(2, [("body".into(), Value::Str("discarded".into()))].into())
            .unwrap();
        index
            .add_document(2, [("body".into(), "discarded".into())].into())
            .unwrap();
        backend.rollback_transaction().unwrap();
        assert_eq!(docs.len().unwrap(), 1, "{mode}");
        assert_eq!(index.doc_freq("body", "discarded").unwrap(), 0, "{mode}");
        let bytes = control.memory().used();
        drop(docs);
        assert!(control.memory().used() < bytes, "{mode}");
        assert_eq!(index.doc_freq("body", "word").unwrap(), 1, "{mode}");
        drop(index);
        assert_eq!(control.memory().used(), 0, "{mode}");
    }
}

#[test]
fn sqlite_controlled_handle_rejection_retires_all_unpublished_allocations() {
    for mode in ["native", "kv", "legacy"] {
        let backend = backend(mode);
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(1 << 20);
            let (name, default) = input(&control);
            let block = control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())
                .unwrap();
            if cancel {
                control.cancellation().cancel();
            }
            let docs = backend.document_store_with_control(name.clone(), &control);
            let index =
                backend.inverted_index_with_control(name.clone(), default.clone(), &control);
            for error in [docs.err().unwrap(), index.err().unwrap()] {
                assert!(
                    if cancel {
                        matches!(error, uqa_storage::StorageBackendError::Cancelled(_))
                    } else {
                        matches!(error, uqa_storage::StorageBackendError::Memory(_))
                    },
                    "{mode}"
                );
            }
            assert_eq!(control.memory().used(), control.memory().limit(), "{mode}");
            drop((name, default, block));
            assert_eq!(control.memory().used(), 0, "{mode}");
        }
    }
}
