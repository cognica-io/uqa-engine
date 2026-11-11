//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::relation_acl::{key as encode_key, RelationAclRecords, RelationAclTuple};

fn ordinary(key: &str) -> StorageBackendResult<(RelationIdentity, Option<String>)> {
    let tuple = RelationAclTuple::new(Some(Vec::new()))?;
    let mut records = RelationAclRecords::default();
    records.insert(key, &serde_json::to_vec(&tuple)?)?;
    let (relation, entries) = records.0.into_iter().next().unwrap();
    let (column, _) = entries.into_iter().next().unwrap();
    Ok((relation, column))
}

#[test]
fn catalog_acl_key_input_preserves_canonical_names_and_exact_admission() {
    for schema in ["public", "", "한🙂\0\n\"\\/", &"schema".repeat(1024)] {
        for name in ["table", "", "quoted.:name"] {
            for column in [None, Some(""), Some("한🙂\0\t\"\\/")] {
                let relation = RelationIdentity::new(schema, name);
                let key = encode_key(&relation, column);
                let expected = ordinary(&key).unwrap();
                let control = StorageReadControl::with_limit(65536);
                let mut decoded = None;
                let measured = allocation_counter::measure(|| {
                    decoded = Some(decode_key_with_control(&key, &control).unwrap());
                });
                let decoded = decoded.unwrap();
                assert_eq!(*decoded, expected);
                assert_eq!(measured.bytes_current, control.memory().used() as i64);
                assert_eq!(decoded.reserved_bytes(), control.memory().used());
                assert_eq!(
                    decoded.reserved_bytes(),
                    decoded.0.schema.capacity()
                        + decoded.0.name.capacity()
                        + decoded.1.as_ref().map_or(0, String::capacity)
                );
                drop(decoded);
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

#[test]
fn catalog_acl_key_input_rejects_noncanonical_relations_and_columns_in_original_order() {
    let relation = r#"{"schema":"public","name":"table"}"#;
    let mut keys = vec![String::new(), "other-prefix".into(), METADATA_PREFIX.into()];
    for relation in [
        "null",
        "{}",
        r#"{"schema":"public"}"#,
        r#"["public","table"]"#,
        r#"{"name":"table","schema":"public"}"#,
        r#"{ "schema":"public","name":"table"}"#,
        r#"{"schema":"\u0070ublic","name":"table"}"#,
        r#"{"schema":"public","name":"table","ignored":[1e9999,"\ud800"]}"#,
    ] {
        keys.push(format!("{METADATA_PREFIX}{relation}:null"));
    }
    for column in [
        "",
        "1",
        "false",
        "{}",
        "[]",
        " null",
        "null ",
        "null!",
        "\"a\\/b\"",
        "\"\\ud800\"",
        "\"\\u0061\"",
    ] {
        keys.push(format!("{METADATA_PREFIX}{relation}:{column}"));
    }
    keys.push(format!("{METADATA_PREFIX}{relation} :null"));
    for key in keys {
        assert!(ordinary(&key).is_err(), "{key}");
        let control = StorageReadControl::with_limit(65536);
        assert!(decode_key_with_control(&key, &control).is_err(), "{key}");
        assert_eq!(control.memory().used(), 0);
    }
    for (suffix, message) in [
        (
            r#"{"schema":"public","schema":!"#,
            "duplicate field `schema`",
        ),
        ("{}:!", "missing field `schema`"),
        (r#"{"schema":"public"}:!"#, "missing field `name`"),
        (
            r#"{"name":"table","schema":"public"}:!"#,
            "ACL tuple belongs to another relation",
        ),
        (
            r#"{"schema":"public","name":"table"}:"\u0061""#,
            "noncanonical ACL tuple key",
        ),
    ] {
        let key = format!("{METADATA_PREFIX}{suffix}");
        assert!(ordinary(&key).unwrap_err().to_string().contains(message));
        let control = StorageReadControl::with_limit(65536);
        assert!(decode_key_with_control(&key, &control)
            .unwrap_err()
            .to_string()
            .contains(message));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_acl_key_input_retires_partial_names_and_preserves_sibling_admission() {
    let key = encode_key(&RelationIdentity::new("한글", "table"), Some("column\n"));
    for end in (0..key.len()).filter(|end| key.is_char_boundary(*end)) {
        let control = StorageReadControl::with_limit(65536);
        assert!(ordinary(&key[..end]).is_err());
        assert!(decode_key_with_control(&key[..end], &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let key = encode_key(
        &RelationIdentity::new("s".repeat(4096), "t".repeat(4096)),
        Some("c"),
    );
    for available in [1, 5, 31, 64, 255, 4096, 8192] {
        let control = StorageReadControl::with_limit(available + 19);
        let sibling = control.memory().reserve(19).unwrap();
        assert!(matches!(
            decode_key_with_control(&key, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), sibling.bytes());
        drop(sibling);
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(65536);
    control.cancellation().cancel();
    assert!(matches!(
        decode_key_with_control(&key, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_acl_key_input_denies_large_names_before_copying_or_reserializing() {
    let key = encode_key(&RelationIdentity::new("public", "x".repeat(4 << 20)), None);
    let control = StorageReadControl::with_limit(256);
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(decode_key_with_control(&key, &control));
    });
    assert!(matches!(
        result.unwrap(),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(measured.bytes_max < 8192, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(control.memory().used(), 0);
}
