//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fields() -> Arc<Document> {
    let mut text = String::with_capacity(8192);
    text.push('x');
    Arc::new([("body".into(), Value::Str(text))].into())
}

#[test]
fn shared_fields_keep_one_charge_and_original_value_addresses() {
    let control = StorageReadControl::with_limit(1 << 20);
    let original = fields();
    let retained = RetainedDocumentFields::new(Arc::clone(&original), &control).unwrap();
    let used = control.memory().used();
    assert!(used >= 8192);
    assert!(std::ptr::eq(
        &raw const original["body"],
        &raw const retained["body"]
    ));
    let sibling = retained.clone();
    assert_eq!(control.memory().used(), used);
    drop(original);
    drop(retained);
    assert_eq!(control.memory().used(), used);
    drop(sibling);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_adoption_releases_partial_charge_without_changing_caller_fields() {
    let control = StorageReadControl::with_limit(2048);
    let original = fields();
    assert!(matches!(
        RetainedDocumentFields::new(Arc::clone(&original), &control),
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(original["body"], Value::Str("x".into()));
    assert_eq!(Arc::strong_count(&original), 1);
    control.cancellation().cancel();
    assert!(matches!(
        RetainedDocumentFields::new(Arc::clone(&original), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unique_output_moves_fields_while_shared_output_keeps_its_sibling_charged() {
    let control = StorageReadControl::with_limit(1 << 20);
    let retained = RetainedDocumentFields::new(fields(), &control).unwrap();
    let Value::Str(text) = &retained["body"] else {
        panic!()
    };
    let address = text.as_ptr();
    let output = retained.into_document();
    let Value::Str(text) = &output["body"] else {
        panic!()
    };
    assert_eq!(text.as_ptr(), address);
    assert_eq!(control.memory().used(), 0);

    let retained = RetainedDocumentFields::new(fields(), &control).unwrap();
    let used = control.memory().used();
    let mut output = retained.clone().into_document();
    output.insert("body".into(), Value::Null);
    assert_eq!(retained["body"], Value::Str("x".into()));
    assert_eq!(control.memory().used(), used);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}
