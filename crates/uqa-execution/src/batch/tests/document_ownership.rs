//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::batch::materialization::SelectedRowMaterializer;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    document_store::{Document, SharedDocumentRow},
    read_control::StorageReadControl,
    DocumentStore, MemoryDocumentStore, MemoryDocumentStoreBuilder, StoredDocument,
};

fn source(control: &StorageReadControl) -> (MemoryDocumentStore, *const u8) {
    let selected = "selected".repeat(8192);
    let pointer = selected.as_ptr();
    let value = Value::Map(Document::from([
        ("selected".into(), Value::Str(selected)),
        ("unused".into(), Value::Str("unused".repeat(32768))),
    ]));
    let memory = value
        .reserve_retained_payload(control.memory(), control.cancellation())
        .unwrap();
    let Value::Map(fields) = value else {
        unreachable!()
    };
    let store = MemoryDocumentStoreBuilder::new(control)
        .add_document(7, Budgeted::new(StoredDocument::new(fields), memory))
        .unwrap()
        .finish()
        .unwrap();
    (store, pointer)
}

fn read(store: &MemoryDocumentStore) -> SharedDocumentRow {
    store
        .get_shared_fields(&[7], &["selected", "unused", "missing"])
        .unwrap()
        .unwrap()
        .pop()
        .unwrap()
        .unwrap()
}

fn text(value: &Value) -> &str {
    let Value::Str(value) = value else {
        panic!("expected original text")
    };
    value
}

#[test]
fn native_document_physical_readers_retain_input_through_projection_and_source_retirement() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (mut store, pointer) = source(&control);
    let row = PhysicalRow::from_document_row(read(&store));
    let selected = row.project_slots(&[2, 0, 0]);
    let joined = PhysicalRow::concat(&PhysicalRow::nulls(2), &selected);
    let prefix = joined.clone().into_prefix(4);
    store.clear().unwrap();
    drop(store);
    drop(row);
    drop(selected);
    drop(joined);
    assert!(control.memory().used() >= 64 << 10);
    for slot in 0..3 {
        assert_eq!(prefix.value(slot), Some(&Value::Null));
    }
    assert_eq!(text(prefix.value(3).unwrap()).as_ptr(), pointer);
    drop(prefix);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_document_selected_output_moves_original_payload_after_row_aliases_retire() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (store, pointer) = source(&control);
    let row = PhysicalRow::from_document_row(read(&store));
    let alias = row.project_slots(&[0]);
    let joined = PhysicalRow::concat(&row, &alias);
    drop(store);
    drop(row);
    drop(alias);
    let schema = RowSchema::new(vec![
        "selected".into(),
        "unused".into(),
        "missing".into(),
        "alias".into(),
    ]);
    let schema = RowSchema::select(&schema, &[("selected".into(), "selected".into())]);
    let batch = Batch::from_physical_rows(schema, vec![joined]);
    let mut output = Vec::new();
    let allocations = allocation_counter::measure(|| output = batch.into_result_rows());
    assert_eq!(text(&output[0]["selected"]).as_ptr(), pointer);
    assert_eq!(text(&output[0]["selected"]), "selected".repeat(8192));
    assert_eq!(control.memory().used(), 0);
    assert!(allocations.bytes_total <= 8192, "{allocations:?}");
}

#[test]
fn native_document_selected_output_keeps_unconsumed_input_admitted_during_unwind() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (store, pointer) = source(&control);
    let row = PhysicalRow::from_document_row(read(&store));
    drop(store);
    let schema = RowSchema::new(vec!["selected".into(), "unused".into(), "missing".into()]);
    let materializer = SelectedRowMaterializer::new(&schema, &[Some(0)]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        materializer.materialize(row, |slot, value| {
            assert_eq!(slot, 0);
            assert_eq!(text(&value).as_ptr(), pointer);
            assert!(control.memory().used() >= 64 << 10);
            panic!("result consumer failed");
        });
    }));
    assert!(result.is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_document_cached_physical_projection_has_no_per_row_allocation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let (store, pointer) = source(&control);
    let shared = read(&store);
    let mut held = None;
    let allocations = allocation_counter::measure(|| {
        for _ in 0..128 {
            held = Some(PhysicalRow::from_document_row(shared.clone()));
        }
    });
    assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    let held = held.unwrap();
    assert_eq!(text(held.value(0).unwrap()).as_ptr(), pointer);
    assert_eq!(held.value(2), Some(&Value::Null));
    drop(shared);
    drop(store);
    assert!(control.memory().used() > 0);
    drop(held);
    assert_eq!(control.memory().used(), 0);
}
