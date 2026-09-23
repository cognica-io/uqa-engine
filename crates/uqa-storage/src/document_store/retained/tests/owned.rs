//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn text_address(fields: &Document) -> *const u8 {
    let Value::Str(text) = &fields["body"] else {
        panic!("text field");
    };
    text.as_ptr()
}

#[test]
fn unique_owned_fields_transfer_the_original_capacity_and_payload_lease() {
    let control = StorageReadControl::with_limit(16 << 10);
    let retained = RetainedDocumentFields::new(fields(), &control).unwrap();
    let address = text_address(&retained);
    let used = control.memory().used();
    let owned = retained.into_budgeted(&control).unwrap();
    assert_eq!(text_address(&owned), address);
    assert_eq!(
        owned.reserved_bytes(),
        used - size_of::<Document>() - size_of::<Budgeted<Arc<Document>>>()
    );
    assert_eq!(control.memory().used(), owned.reserved_bytes());
    assert!(owned.reserved_bytes() >= 8192);
    drop(owned);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn shared_owned_fields_copy_under_the_allowance_without_adopting_spare_capacity() {
    for shared_wrapper in [false, true] {
        let control = StorageReadControl::with_limit(32 << 10);
        let original = fields();
        let address = text_address(&original);
        let retained = RetainedDocumentFields::new(Arc::clone(&original), &control).unwrap();
        let sibling = shared_wrapper.then(|| retained.clone());
        let used = control.memory().used();
        if shared_wrapper {
            drop(original);
        }
        let owned = retained.into_budgeted(&control).unwrap();
        assert_ne!(text_address(&owned), address);
        assert_eq!(owned["body"], Value::Str("x".into()));
        assert!(owned.reserved_bytes() < 8192);
        assert_eq!(
            control.memory().used(),
            owned.reserved_bytes() + if shared_wrapper { used } else { 0 }
        );
        if let Some(sibling) = sibling {
            assert_eq!(text_address(&sibling), address);
            assert_eq!(sibling["body"], Value::Str("x".into()));
            drop(owned);
            assert_eq!(control.memory().used(), used);
            drop(sibling);
        } else {
            drop(owned);
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn rejected_shared_copy_keeps_the_existing_reader_and_releases_partial_output() {
    let control = StorageReadControl::with_limit(12 << 10);
    let fields = Arc::new([("body".into(), Value::Str("x".repeat(8192)))].into());
    let retained = RetainedDocumentFields::new(fields, &control).unwrap();
    let address = text_address(&retained);
    let used = control.memory().used();
    assert!(matches!(
        retained.clone().into_budgeted(&control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), used);
    assert_eq!(text_address(&retained), address);
    assert_eq!(retained["body"], Value::Str("x".repeat(8192)));
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn owned_transfer_rejects_foreign_allowances_and_cancellation_before_copying() {
    for cancelled in [false, true] {
        let owner = StorageReadControl::with_limit(16 << 10);
        let other = StorageReadControl::with_limit(16 << 10);
        let retained = RetainedDocumentFields::new(fields(), &owner).unwrap();
        if cancelled {
            owner.cancellation().cancel();
            assert!(matches!(
                retained.into_budgeted(&owner),
                Err(StorageBackendError::Cancelled(_))
            ));
        } else {
            assert!(matches!(
                retained.into_budgeted(&other),
                Err(StorageBackendError::Other(_))
            ));
        }
        assert_eq!(owner.memory().used(), 0);
        assert_eq!(other.memory().used(), 0);
    }
}
