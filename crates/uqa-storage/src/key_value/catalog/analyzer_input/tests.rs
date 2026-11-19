//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{key_value::codec::push_str, KeyValueStore, MemoryKeyValueStore};
use std::sync::Arc;

fn equivalent(key: &[u8], value: &[u8], is_binding: bool) {
    let store = Arc::new(MemoryKeyValueStore::new());
    store.put(key, value).unwrap();
    let catalog = KeyValueCatalog::new(store);
    let control = StorageReadControl::with_limit(1 << 20);
    if is_binding {
        let expected = catalog.load_table_field_analyzer_bindings_impl();
        match (expected, binding(key, value, &control)) {
            (Ok(expected), Ok(Some(actual))) => {
                assert_eq!(expected.len(), 1);
                assert_eq!(
                    (&actual.table, &actual.field, &actual.binding_json),
                    (&expected[0].0, &expected[0].1, &expected[0].2)
                );
            }
            (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
            (expected, actual) => panic!("{key:?}: {expected:?} / {actual:?}"),
        }
    } else {
        let expected = catalog.load_table_field_analyzers_impl();
        match (expected, label(key, value, &control)) {
            (Ok(expected), Ok(Some(actual))) => {
                assert_eq!(expected.len(), 1);
                assert_eq!(
                    (
                        &actual.table,
                        &actual.field,
                        &actual.phase,
                        &actual.analyzer_name
                    ),
                    (
                        &expected[0].0,
                        &expected[0].1,
                        &expected[0].2,
                        &expected[0].3
                    )
                );
            }
            (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
            (expected, actual) => panic!("{key:?}: {expected:?} / {actual:?}"),
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_value_analyzer_input_preserves_original_keys_text_and_trailing_key_rules() {
    for (table, field, phase) in [
        ("", "", ""),
        ("\"Schema.Name\".\"Table\"", "field\0한글", "INDEX"),
    ] {
        for is_binding in [false, true] {
            let mut key = vec![if is_binding {
                TAG_FIELD_ANALYZER_BINDING
            } else {
                TAG_TABLE_FIELD_ANALYZER
            }];
            push_str(&mut key, table).unwrap();
            push_str(&mut key, field).unwrap();
            if !is_binding {
                push_str(&mut key, phase).unwrap();
            }
            for suffix in [b"".as_slice(), b"ignored suffix"] {
                let mut key = key.clone();
                key.extend_from_slice(suffix);
                for value in [
                    b"".as_slice(),
                    " { \"n\": 1.00, \"s\": \"한글\\u0000\" } ".as_bytes(),
                    &[0xff],
                ] {
                    equivalent(&key, value, is_binding);
                }
            }
            for length in 1..key.len() {
                equivalent(&key[..length], &[0xff], is_binding);
            }
            let final_byte = key.len() - 1;
            if !key.is_empty() && key[final_byte] != 0 {
                key[final_byte] = 0xff;
                equivalent(&key, b"value", is_binding);
            }
        }
    }
}

#[test]
fn key_value_analyzer_input_retains_no_failed_or_cancelled_row_allocations() {
    let mut key = vec![TAG_FIELD_ANALYZER_BINDING];
    push_str(&mut key, "table").unwrap();
    push_str(&mut key, &"field한글".repeat(1024)).unwrap();
    let value = " { \"x\": 1.00 } ".repeat(4096);
    let control = StorageReadControl::with_limit(1 << 20);
    let row = binding(&key, value.as_bytes(), &control).unwrap().unwrap();
    let peak = control.memory().peak();
    assert_eq!(row.binding_json, value);
    assert_eq!(row.reserved_bytes(), control.memory().used());
    drop(row);
    for limit in (0..peak).step_by(1024).chain([peak - 1]) {
        let denied = StorageReadControl::with_limit(limit);
        assert!(binding(&key, value.as_bytes(), &denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(binding(&key, value.as_bytes(), &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let control = StorageReadControl::with_limit(65536);
    assert!(catalog
        .analyzer_input(
            CatalogInputLimits {
                maximum_key_bytes: 4096,
                maximum_value_bytes: 4096
            },
            &control
        )
        .is_err());
    assert_eq!(control.memory().used(), 0);
}
