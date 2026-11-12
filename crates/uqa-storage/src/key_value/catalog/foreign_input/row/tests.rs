//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::catalog::{relation_key, KeyValueCatalog, TAG_FOREIGN_TABLE};
use crate::{CatalogFacade, KeyValueStore, MemoryKeyValueStore, RelationSecurityRow};
use std::sync::Arc;

fn envelope(fields: &str) -> String {
    format!(
        r#"{{"security_version":1,"server_name":"source","columns_json":"[ ]","options_json":"{{ }}",{fields}}}"#
    )
}

fn decode(
    text: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<ForeignTableRow>> {
    let key = relation_key(
        TAG_FOREIGN_TABLE,
        &RelationIdentity::new("public", "foreign"),
    )
    .unwrap();
    super::super::decode(&key, text.as_bytes(), control).map(Option::unwrap)
}

fn compare(text: &str) {
    let key = relation_key(
        TAG_FOREIGN_TABLE,
        &RelationIdentity::new("public", "foreign"),
    )
    .unwrap();
    let store = Arc::new(MemoryKeyValueStore::new());
    store.put(&key, text.as_bytes()).unwrap();
    let catalog = KeyValueCatalog::new(store);
    let control = StorageReadControl::with_limit(1 << 20);
    match (catalog.load_foreign_tables(), decode(text, &control)) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual.relation, expected[0].relation);
            assert_eq!(actual.security, expected[0].security);
            assert_eq!(actual.server_name, expected[0].server_name);
            assert_eq!(actual.columns_json, expected[0].columns_json);
            assert_eq!(actual.options_json, expected[0].options_json);
        }
        (Err(expected), Err(actual)) => {
            assert_eq!(
                std::mem::discriminant(&expected),
                std::mem::discriminant(&actual),
                "{text}: {expected} / {actual}"
            );
            if matches!(expected, StorageBackendError::Other(_)) {
                assert_eq!(expected.to_string(), actual.to_string());
            }
        }
        (expected, actual) => panic!("{text}: {expected:?} / {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn foreign_catalog_envelopes_preserve_flattened_authority_fields_and_version_precedence() {
    let security = serde_json::to_value(RelationSecurityRow::bootstrap()).unwrap();
    let bound = security
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| format!("{name:?}:{value}"))
        .collect::<Vec<_>>()
        .join(",");
    for fields in [
        bound.as_str(),
        r#""role_owner":"owner""#,
        r#""role_owner":false,"role_owner":"owner""#,
        r#""role_owner":1e9999,"role_owner":"owner""#,
        r#""role_owner":"\ud800","role_owner":"owner""#,
        r#""role_owner":"owner","column_acls":{"x":1e9999,"x":[]}"#,
        r#""role_owner":"owner","column_acls":false,"column_acls":{}"#,
        r#""role_owner":"owner","server_name":"duplicate""#,
        r#""role_owner":"owner","unexpected":null"#,
        r#""relation_security_format":1,"role_owner":{},"acl":null,"column_acls":{}"#,
    ] {
        for version in [
            "0", "1", "2", "255", "256", "-0", "1.0", "1e0", "null", "1e9999",
        ] {
            compare(&envelope(fields).replacen(
                "\"security_version\":1",
                &format!("\"security_version\":{version}"),
                1,
            ));
        }
    }
    for malformed in ["{}", "[]", "null", "[1,\"owner\",\"source\",\"[]\",\"{}\"]"] {
        compare(malformed);
    }
}

#[test]
fn foreign_catalog_envelopes_retain_exact_payload_allocations_and_retire_denied_prefixes() {
    let text = envelope(r#""role_owner":"owner""#).replace("[ ]", &"x".repeat(65536));
    let key = relation_key(
        TAG_FOREIGN_TABLE,
        &RelationIdentity::new("public", "foreign"),
    )
    .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(
            super::super::decode(&key, text.as_bytes(), &control)
                .unwrap()
                .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(allocation.bytes_current).unwrap(),
        control.memory().used()
    );
    assert_eq!(output.as_ref().unwrap().columns_json.len(), 65536);
    let peak = control.memory().peak();
    drop(output);
    assert_eq!(control.memory().used(), 0);
    for limit in (0..peak).step_by(997).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        assert!(decode(&text, &control).is_err(), "limit {limit}");
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    control.cancellation().cancel();
    assert!(decode(&text, &control).is_err());
    assert_eq!(control.memory().peak(), 0);
}

#[test]
fn foreign_catalog_envelopes_borrow_overwritten_arbitrary_precision_numbers() {
    let text = envelope(&format!(
        r#""role_owner":{},"role_owner":"owner""#,
        "9".repeat(2 << 20)
    ));
    compare(&text);
    let control = StorageReadControl::with_limit(8192);
    let allocation = allocation_counter::measure(|| drop(decode(&text, &control).unwrap()));
    assert_eq!(control.memory().used(), 0);
    assert!(allocation.bytes_max < 8192, "{allocation:?}");
}

#[test]
fn foreign_catalog_envelopes_reject_every_truncated_native_record() {
    let text = envelope(r#""role_owner":"owner""#);
    for end in 0..text.len() {
        compare(&text[..end]);
    }
    let mut bytes = text.into_bytes();
    for position in 0..bytes.len() {
        let old = std::mem::replace(&mut bytes[position], b'!');
        compare(std::str::from_utf8(&bytes).unwrap());
        bytes[position] = old;
    }
}
