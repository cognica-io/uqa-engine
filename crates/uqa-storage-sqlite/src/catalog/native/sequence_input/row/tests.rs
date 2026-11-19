//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::sequences_views::codec::decode_native_sequence_row;
use rusqlite::types::Value;

fn fixture() -> Vec<Value> {
    let mut role = vec![1];
    role.extend_from_slice(&10_u32.to_be_bytes());
    role.extend_from_slice(&uqa_core::catalog_role::RoleIdentity::BOOTSTRAP.object_id);
    vec![
        Value::Text("public".into()),
        Value::Text("sequence한글".into()),
        Value::Text("ignored".into()),
        Value::Integer(1),
        Value::Integer(2),
        Value::Integer(7),
        Value::Integer(1),
        Value::Text("p".into()),
        Value::Blob(vec![3; 16]),
        Value::Text("bigint".into()),
        Value::Integer(1),
        Value::Integer(1000),
        Value::Integer(0),
        Value::Integer(3),
        Value::Blob(vec![4; 16]),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Blob(role),
        Value::Null,
        Value::Integer(0),
    ]
}

fn equivalent(values: &[Value]) {
    let row = values.iter().map(ValueRef::from).collect::<Vec<_>>();
    let control = StorageReadControl::with_limit(1 << 20);
    match (decode_native_sequence_row(&row), read(&row, &control)) {
        (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected),
        (Err(expected), Err(actual)) => {
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
            if matches!(expected, SQLiteError::StorageBackend(_)) {
                assert_eq!(actual.to_string(), expected.to_string());
            }
        }
        (expected, actual) => panic!("{values:?}: {expected:?} / {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_sequence_input_preserves_original_cell_validation_and_authority_order() {
    equivalent(&fixture());
    for column in 0..21 {
        for value in [
            Value::Null,
            Value::Integer(0),
            Value::Real(1.5),
            Value::Blob(vec![0; 3]),
            Value::Text("{\"ignored\":true}".into()),
        ] {
            let mut row = fixture();
            row[column] = value;
            equivalent(&row);
        }
    }
    for owner in [
        Value::Text("owner한글".into()),
        Value::Integer(0),
        Value::Blob(vec![0; 21]),
    ] {
        for acl in [
            Value::Null,
            Value::Text("null".into()),
            Value::Text("[]".into()),
            Value::Text("[{\"role\":\"PUBLIC\"}]".into()),
        ] {
            let mut row = fixture();
            row[18] = owner.clone();
            row[19] = acl;
            row[15] = Value::Blob(vec![5; 16]);
            equivalent(&row);
        }
    }
}

#[test]
fn native_sequence_input_retains_owned_text_and_releases_denied_prefixes() {
    let values = fixture();
    let row = values.iter().map(ValueRef::from).collect::<Vec<_>>();
    let control = StorageReadControl::with_limit(65536);
    let output = read(&row, &control).unwrap();
    let peak = control.memory().peak();
    assert_eq!(output.reserved_bytes(), control.memory().used());
    drop(output);
    assert_eq!(control.memory().used(), 0);
    for limit in (0..peak).step_by(7).chain([peak - 1]) {
        let denied = StorageReadControl::with_limit(limit);
        assert!(read(&row, &denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
}
