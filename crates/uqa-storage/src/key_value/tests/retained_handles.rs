//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{inverted_index::AnalyzerDefault, read_control::StorageReadControl, StorageName};
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};
use uqa_core::memory::{Budgeted, BudgetedString};

fn name(control: &StorageReadControl) -> StorageName {
    let mut value = BudgetedString::new(control.memory());
    value.push_str("public.notes").unwrap();
    let (value, memory) = value.into_parts();
    StorageName::from_admitted(Budgeted::new(value, memory)).unwrap()
}

fn default(control: &StorageReadControl) -> AnalyzerDefault {
    let bytes = serde_json::to_vec(&uqa_analysis::whitespace_analyzer()).unwrap();
    let memory = control.memory().reserve(bytes.capacity()).unwrap();
    let definition =
        AnalyzerDefinition::from_image(Budgeted::new(bytes, memory), control.cancellation())
            .unwrap();
    AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), control).unwrap()
}

#[test]
fn controlled_key_value_handles_remain_writable_after_input_retirement() {
    let backend = super::super::KeyValueStorageBackend::new(store());
    let control = StorageReadControl::with_limit(1 << 20);
    let name = name(&control);
    let default = default(&control);
    let mut docs = backend
        .document_store_with_control(name.clone(), &control)
        .unwrap();
    let mut index = backend
        .inverted_index_with_control(name.clone(), default, &control)
        .unwrap();
    drop(name);
    control.cancellation().cancel();
    docs.put(1, [("body".into(), Value::Str("First WORD".into()))].into())
        .unwrap();
    index
        .add_document(1, [("body".into(), "First WORD".into())].into())
        .unwrap();
    assert_eq!(
        docs.get_field(1, "body").unwrap(),
        Some(Value::Str("First WORD".into()))
    );
    assert_eq!(index.doc_freq("body", "word").unwrap(), 1);
    let retained = control.memory().used();
    assert!(retained > 0);
    drop(docs);
    assert!(control.memory().used() < retained);
    assert_eq!(index.doc_freq("body", "first").unwrap(), 1);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_key_value_handle_admission_retires_rejected_input_and_headers() {
    let backend = super::super::KeyValueStorageBackend::new(store());
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let name = name(&control);
        let default = default(&control);
        let before = control.memory().used();
        let block = control
            .memory()
            .reserve(control.memory().limit() - before)
            .unwrap();
        if cancel {
            control.cancellation().cancel();
        }
        let docs = backend.document_store_with_control(name.clone(), &control);
        let index = backend.inverted_index_with_control(name.clone(), default.clone(), &control);
        for error in [docs.err().unwrap(), index.err().unwrap()] {
            assert!(if cancel {
                matches!(error, StorageBackendError::Cancelled(_))
            } else {
                matches!(error, StorageBackendError::Memory(_))
            });
        }
        assert_eq!(control.memory().used(), control.memory().limit());
        drop((block, name, default));
        assert_eq!(control.memory().used(), 0);
    }
}
