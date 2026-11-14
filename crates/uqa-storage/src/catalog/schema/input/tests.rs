//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn current() -> String {
    let mut value = super::super::BoundSchemaRow::bootstrap("public");
    value.tuple = Some(SchemaTupleIdentity::initial(2200));
    serde_json::to_string(&SchemaRow::Bound(value)).unwrap()
}

fn equivalent(bytes: &[u8]) {
    let control = StorageReadControl::with_limit(1 << 20);
    match (
        serde_json::from_slice::<SchemaRow>(bytes),
        row(bytes, &control),
    ) {
        (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{bytes:?}"),
        (Err(_), Err(_)) => (),
        (expected, actual) => panic!("{bytes:?}: expected {expected:?}, actual {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn schema_envelope_input_matches_original_format_selection_and_projected_values() {
    let current = current();
    for text in [
        current.clone(),
        current.replace(
            "\"schema_security_format\":2",
            "\"schema_security_format\":false,\"schema_security_format\":2",
        ),
        current.replace(
            "\"schema_security_format\":2",
            "\"schema_security_format\":1",
        ),
        current.replace("\"schema_security_format\":2,", ""),
        current.replace("\"oid\":2200", "\"oid\":0"),
        current.replace("\"name\":\"public\"", "\"name\":false,\"name\":\"public\""),
        serde_json::to_string(&SchemaRow::bootstrap("public")).unwrap(),
        serde_json::to_string(&SchemaRow::legacy("public")).unwrap(),
        r#"{"name":"old","ignored":{"overwritten":0,"overwritten":"\ud800"}}"#.into(),
        r#"{"name":"old","ignored":1e9999}"#.into(),
        r#"{"name":"old","tuple":null}"#.into(),
        r#"["old"]"#.into(),
        r#"["old","owner",null]"#.into(),
        "null".into(),
    ] {
        equivalent(text.as_bytes());
    }
    for version in ["-0", "1.0", "2e0", "3", "-1", "null", "true", "\"2\""] {
        equivalent(
            current
                .replace(
                    "\"schema_security_format\":2",
                    &format!("\"schema_security_format\":{version}"),
                )
                .as_bytes(),
        );
    }
}

#[test]
fn schema_envelope_input_rejects_original_truncations_and_malformed_bytes() {
    for text in [
        current(),
        serde_json::to_string(&SchemaRow::legacy("public")).unwrap(),
    ] {
        for end in 0..text.len() {
            equivalent(&text.as_bytes()[..end]);
        }
        for position in 0..text.len() {
            for replacement in [0, b'"', b'}', b'[', b'9', 255] {
                let mut bytes = text.as_bytes().to_vec();
                bytes[position] = replacement;
                equivalent(&bytes);
            }
        }
    }
}

#[test]
fn schema_envelope_input_accounts_for_native_fields_and_retires_denied_prefixes() {
    for text in [
        current(),
        serde_json::to_string(&SchemaRow::legacy("public")).unwrap(),
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(row(text.as_bytes(), &control).unwrap());
        });
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            control.memory().used()
        );
        assert!(usize::try_from(allocations.bytes_max).unwrap() <= control.memory().peak());
        let peak = control.memory().peak();
        control.cancellation().cancel();
        assert_eq!(output.as_ref().unwrap().name(), "public");
        let allocations = allocation_counter::measure(|| {
            assert!(row(text.as_bytes(), &control).is_err());
        });
        assert_eq!(allocations.bytes_total, 0);
        drop(output);
        assert_eq!(control.memory().used(), 0);
        for limit in (0..peak).step_by(73).chain([peak - 1]) {
            let denied = StorageReadControl::with_limit(limit);
            assert!(row(text.as_bytes(), &denied).is_err());
            assert_eq!(denied.memory().used(), 0);
        }
    }
}
