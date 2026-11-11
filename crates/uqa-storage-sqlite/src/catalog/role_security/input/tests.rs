//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{decode_relation_cells, encode_identity};
use super::*;
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

#[test]
fn native_authority_input_preserves_original_storage_classes_nulls_and_recipient_shapes() {
    let identity = encode_identity(RoleIdentity::BOOTSTRAP).unwrap();
    let owner_json = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    let bound =
        format!(r#"[{{"role":null,"grantor":{owner_json},"privileges":{{"select":true}}}}]"#);
    let named = r#"[{"role":{"kind":"role","name":"PUBLIC"},"grantor":"owner"}]"#;
    let bound_columns = format!(r#"{{"value":{bound}}}"#);
    let named_columns = format!(r#"{{"value":{named}}}"#);
    for owner in [
        ValueRef::Text(b"owner"),
        (&identity).into(),
        ValueRef::Blob(b"invalid"),
        ValueRef::Null,
        ValueRef::Integer(1),
        ValueRef::Text(&[255]),
    ] {
        for acl in [
            ValueRef::Null,
            ValueRef::Text(b"null"),
            ValueRef::Text(b"[]"),
            ValueRef::Text(bound.as_bytes()),
            ValueRef::Text(named.as_bytes()),
            ValueRef::Integer(2),
            ValueRef::Text(b"["),
        ] {
            for columns in [
                ValueRef::Null,
                ValueRef::Text(b"null"),
                ValueRef::Text(b"{}"),
                ValueRef::Text(bound_columns.as_bytes()),
                ValueRef::Text(named_columns.as_bytes()),
                ValueRef::Integer(3),
            ] {
                let control = StorageReadControl::with_limit(65536);
                let expected = decode_relation_cells(owner, acl, columns);
                let actual = relation(owner, acl, columns, &control);
                match (expected, actual) {
                    (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected),
                    (Err(expected), Err(actual)) => {
                        assert_eq!(
                            std::mem::discriminant(&actual),
                            std::mem::discriminant(&expected),
                            "{owner:?}, {acl:?}, {columns:?}: {actual} / {expected}"
                        );
                        if matches!(expected, SQLiteError::StorageBackend(_)) {
                            assert_eq!(actual.to_string(), expected.to_string());
                        }
                    }
                    (expected, actual) => {
                        panic!("{owner:?}, {acl:?}, {columns:?}: {actual:?} / {expected:?}")
                    }
                }
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

#[test]
fn native_authority_input_retains_exact_admitted_cells_and_retires_partial_failures() {
    let owner = "owner한🙂".repeat(64);
    let acl = r#"[{"role":"reader","grantor":"owner"}]"#;
    let columns = format!(r#"{{"column":{acl}}}"#);
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    let control = StorageReadControl::new(&memory, &cancellation);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            relation(
                ValueRef::Text(owner.as_bytes()),
                ValueRef::Text(acl.as_bytes()),
                ValueRef::Text(columns.as_bytes()),
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(measured.bytes_current, output.reserved_bytes() as i64);
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(memory.used(), 0);
    for end in 0..columns.len() {
        assert!(relation(
            ValueRef::Text(owner.as_bytes()),
            ValueRef::Text(acl.as_bytes()),
            ValueRef::Text(&columns.as_bytes()[..end]),
            &control
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    for limit in [1, 63, 255, 1024] {
        let control = StorageReadControl::with_limit(limit + 19);
        let sibling = control.memory().reserve(19).unwrap();
        assert!(matches!(
            relation(
                ValueRef::Text(owner.as_bytes()),
                ValueRef::Text(acl.as_bytes()),
                ValueRef::Text(columns.as_bytes()),
                &control
            ),
            Err(SQLiteError::Memory(_))
        ));
        assert_eq!(control.memory().used(), sibling.bytes());
    }
    cancellation.cancel();
    assert!(matches!(
        relation(
            ValueRef::Text(owner.as_bytes()),
            ValueRef::Null,
            ValueRef::Null,
            &control
        ),
        Err(SQLiteError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
