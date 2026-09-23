//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod owned;

#[test]
fn retained_field_readers_admit_foreign_allowances_without_copying_payloads() {
    let owner = StorageReadControl::with_limit(1 << 20);
    let caller = StorageReadControl::with_limit(1 << 20);
    let original = RetainedDocumentFields::new(fields(), &owner).unwrap();
    let baseline = owner.memory().used();
    let same = original.retain_with_control(&owner).unwrap();
    assert_eq!(owner.memory().used(), baseline);
    let foreign = original.retain_with_control(&caller).unwrap();
    assert!(std::ptr::eq(original.as_ref(), foreign.as_ref()));
    assert!(caller.memory().used() >= 8192);
    let limited = StorageReadControl::with_limit(1024);
    assert!(matches!(
        original.retain_with_control(&limited),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(limited.memory().used(), 0);
    drop((same, original));
    assert_eq!(owner.memory().used(), 0);
    assert_eq!(foreign["body"], Value::Str("x".into()));
    drop(foreign);
    assert_eq!(caller.memory().used(), 0);
}

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

#[test]
fn budgeted_adoption_transfers_its_payload_once_and_keeps_field_addresses() {
    let control = StorageReadControl::with_limit(11_000);
    let mut text = String::with_capacity(8192);
    text.push_str("kept");
    let address = text.as_ptr();
    let fields = Value::Map([("body".into(), Value::Str(text))].into());
    let memory = fields
        .reserve_retained_payload(control.memory(), control.cancellation())
        .unwrap();
    let Value::Map(fields) = fields else { panic!() };
    let retained =
        RetainedDocumentFields::from_budgeted(Budgeted::new(fields, memory), &control).unwrap();
    assert!(control.memory().used() >= 8192);
    assert!(control.memory().used() < 11_000);
    let Value::Str(text) = &retained["body"] else {
        panic!()
    };
    assert_eq!(text.as_ptr(), address);
    let shared = retained.clone();
    let used = control.memory().used();
    drop(retained);
    assert_eq!(control.memory().used(), used);
    drop(shared);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn budgeted_adoption_rejects_foreign_or_incomplete_leases_without_leaking_payloads() {
    let control = StorageReadControl::with_limit(4096);
    let foreign = StorageReadControl::with_limit(4096);
    let fields = Value::Map([("body".into(), Value::Str("text".into()))].into());
    let memory = fields
        .reserve_retained_payload(foreign.memory(), foreign.cancellation())
        .unwrap();
    let Value::Map(fields) = fields else { panic!() };
    assert!(matches!(
        RetainedDocumentFields::from_budgeted(Budgeted::new(fields, memory), &control),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(foreign.memory().used(), 0);

    let fields = [("body".into(), Value::Str("text".into()))].into();
    let memory = control.memory().reserve(1).unwrap();
    assert!(matches!(
        RetainedDocumentFields::from_budgeted(Budgeted::new(fields, memory), &control),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
