//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{RelationSecurityRow, StorageBackendError};
use uqa_core::catalog_role::RoleIdentity;

fn envelope(security: &str) -> String {
    format!(
        r#"{{"relation":{{"schema":"public","name":"items"}},"analyzer_json":"{{}}","fts_fields":["text","text"],"vector_fields":[["v",32]],{security}}}"#
    )
}

fn compare(text: &str) {
    let expected = serde_json::from_str::<TableSchema>(text);
    let control = StorageReadControl::with_limit(1 << 20);
    let actual = row(text.as_bytes(), &control);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(
                serde_json::to_value(&*actual).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "{text}"
            );
            assert_eq!(actual.security, expected.security);
        }
        (Err(_), Err(StorageBackendError::Serde(_))) => (),
        (expected, actual) => panic!("{text}: expected {expected:?}, actual {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_table_row_input_preserves_flattened_security_versions_and_original_fields() {
    let owner = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    let security = serde_json::to_value(RelationSecurityRow::bootstrap()).unwrap();
    let security = security
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key:?}:{value}"))
        .collect::<Vec<_>>()
        .join(",");
    for text in [
        envelope(r#""role_owner":"owner""#),
        envelope(&security),
        envelope(r#""role_owner":"PUBLIC","columns_json":"","constraints_json":"[]""#),
        envelope(&format!(
            r#""relation_security_format":1,"role_owner":{owner},"acl":null,"column_acls":{{}}"#
        )),
    ] {
        compare(&text);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let reversed = format!(
            "{{{}}}",
            value
                .as_object()
                .unwrap()
                .iter()
                .rev()
                .map(|(key, value)| format!("{key:?}:{value}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        compare(&reversed);
    }
    for version in [
        None,
        Some("1"),
        Some("1.0"),
        Some("1e0"),
        Some("-0"),
        Some("null"),
        Some("\"1\""),
        Some("2"),
        Some("-1"),
        Some("1e9999"),
    ] {
        for owner in [r#""owner""#, "{}", owner.as_str()] {
            for acl in [
                None,
                Some("null"),
                Some("[]"),
                Some(r#"[{"role":"PUBLIC"}]"#),
            ] {
                for columns in [None, Some("{}"), Some("null")] {
                    let mut fields = vec![format!(r#""role_owner":{owner}"#)];
                    if let Some(version) = version {
                        fields.push(format!(r#""relation_security_format":{version}"#));
                    }
                    if let Some(acl) = acl {
                        fields.push(format!(r#""acl":{acl}"#));
                    }
                    if let Some(columns) = columns {
                        fields.push(format!(r#""column_acls":{columns}"#));
                    }
                    compare(&envelope(&fields.join(",")));
                }
            }
        }
    }
}

#[test]
fn catalog_table_row_input_preserves_known_duplicates_and_projected_authority_collisions() {
    let zeros = serde_json::to_string(&[0_u8; 16]).unwrap();
    for fields in [
        r#""role_owner":false,"role_owner":"owner""#.to_owned(),
        r#""role_owner":"owner","column_acls":false,"column_acls":{"x":false,"x":[]}"#.to_owned(),
        r#""role_owner":"owner","acl":[{"role":false,"role":"PUBLIC","privileges":{"select":false,"select":true}}]"#.to_owned(),
        format!(r#""relation_security_format":0,"relation_security_format":1,"role_owner":{{"oid":false,"oid":-0,"object_id":{zeros}}},"acl":null,"column_acls":{{}}"#),
        r#""role_owner":"owner","columns_json":null"#.to_owned(),
        r#""role_owner":"owner","analyzer_json":"second""#.to_owned(),
        r#""role_owner":"owner","column_acls":{"x":[{"role":"PUBLIC"}]},"acl_revisions":{}"#.to_owned(),
        r#""role_owner":"owner","unrecognized":true"#.to_owned(),
        r#""role_owner":"\ud800","role_owner":"owner""#.to_owned(),
        r#""role_owner":1e9999,"role_owner":"owner""#.to_owned(),
        r#""role_owner":"owner","column_acls":{"x":1e9999,"x":[]}"#.to_owned(),
    ] { compare(&envelope(&fields)); }
    let text = envelope(r#""role_owner":"owner""#).replace(
        r#""name":"items""#,
        r#""name":"items","ignored":["\ud800",1e9999]"#,
    );
    compare(&text);
    let text = envelope(r#""role_owner":"owner""#).replace(
        r#"["v",32]"#,
        r#"{"field":"v","dimensions":32,"ignored":["\ud800",1e9999]}"#,
    );
    compare(&text);
    for depth in [124, 125, 126, 127, 128] {
        let fields = format!(
            r#""role_owner":"owner","column_acls":{}0{},"column_acls":{{}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        );
        compare(&envelope(&fields));
    }
    for text in ["null", "[]", "{}", r#"{"relation":{}}"#] {
        compare(text);
    }
}

#[test]
fn catalog_table_row_input_keeps_field_error_order_before_later_syntax_and_security() {
    for (text, original, controlled) in [
        (
            r#"{"relation":{"schema":"s","name":"n"},"relation":!"#,
            "duplicate field `relation`",
            "duplicate field `relation`",
        ),
        (
            r#"{"relation":{},"role_owner":!"#,
            "missing field `schema`",
            "missing field `schema`",
        ),
        (
            r#"{"fts_fields":[false,!"#,
            "expected a string",
            "expected a table field name",
        ),
        (
            r#"{"vector_fields":[{"field":false,"dimensions":!"#,
            "expected a string",
            "expected a table field name",
        ),
        (
            r#"{"relation":{"schema":"s","name":"n"},"role_owner":false}"#,
            "missing field `analyzer_json`",
            "missing field `analyzer_json`",
        ),
    ] {
        let expected = serde_json::from_str::<TableSchema>(text).unwrap_err();
        assert!(
            expected.to_string().contains(original),
            "{text}: {expected}"
        );
        let control = StorageReadControl::with_limit(65536);
        let actual = row(text.as_bytes(), &control).unwrap_err();
        assert!(actual.to_string().contains(controlled), "{text}: {actual}");
        assert_eq!(control.memory().used(), 0);
    }
    let text = format!("{} !", envelope(r#""role_owner":false"#));
    let expected = serde_json::from_str::<TableSchema>(&text).unwrap_err();
    assert!(
        expected.to_string().contains("expected a string"),
        "{expected}"
    );
    let control = StorageReadControl::with_limit(65536);
    assert!(row(text.as_bytes(), &control)
        .unwrap_err()
        .to_string()
        .contains("expected a table catalog string"));
}

#[test]
fn catalog_table_row_input_retains_exact_buffers_and_skips_discarded_large_values() {
    let control = StorageReadControl::with_limit(65536);
    let text = envelope(&format!(
        r#""role_owner":"{}","role_owner":"owner","column_acls":{{"field":[{{"role":"reader","grantor":"owner"}}]}}"#,
        "x".repeat(4 << 20)
    ));
    let expected: TableSchema = serde_json::from_str(&text).unwrap();
    let sibling = control.memory().reserve(19).unwrap();
    let mut actual = None;
    let measured =
        allocation_counter::measure(|| actual = Some(row(text.as_bytes(), &control).unwrap()));
    let actual = actual.unwrap();
    assert_eq!(
        serde_json::to_value(&*actual).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(measured.bytes_current, actual.reserved_bytes() as i64);
    assert_eq!(control.memory().used(), actual.reserved_bytes() + 19);
    assert!(measured.bytes_max < 65536, "{measured:?}");
    drop(actual);
    assert_eq!(control.memory().used(), sibling.bytes());
    for field in ["role_owner", "columns_json", "constraints_json"] {
        let text = envelope(&format!(
            r#""role_owner":"owner","{field}":"{}""#,
            "x".repeat(1 << 20)
        ));
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                row(text.as_bytes(), &control),
                Err(StorageBackendError::Memory(_))
            ));
        });
        assert!(measured.bytes_max < 65536, "{field}: {measured:?}");
        assert_eq!(control.memory().used(), sibling.bytes());
    }
}

#[test]
fn catalog_table_row_input_retires_all_truncations_cancellation_and_partial_admission() {
    let text = envelope(
        r#""role_owner":"owner","column_acls":{"column":[{"role":"reader","grantor":"owner"}]}"#,
    );
    for end in 0..text.len() {
        let control = StorageReadControl::with_limit(65536);
        let sibling = control.memory().reserve(19).unwrap();
        assert!(row(&text.as_bytes()[..end], &control).is_err(), "{end}");
        assert_eq!(control.memory().used(), sibling.bytes());
    }
    let original = StorageReadControl::with_limit(65536);
    drop(row(text.as_bytes(), &original).unwrap());
    let peak = original.memory().peak();
    for limit in [1, 31, 63, 127, 255, 511, peak - 1]
        .into_iter()
        .filter(|limit| *limit < peak)
    {
        let control = StorageReadControl::with_limit(limit + 19);
        let sibling = control.memory().reserve(19).unwrap();
        assert!(matches!(
            row(text.as_bytes(), &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), sibling.bytes());
    }
    let exact = StorageReadControl::with_limit(peak);
    drop(row(text.as_bytes(), &exact).unwrap());
    assert_eq!(exact.memory().used(), 0);
    let control = StorageReadControl::with_limit(65536);
    control.cancellation().cancel();
    assert!(matches!(
        row(text.as_bytes(), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
