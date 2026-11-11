//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

#[test]
fn durable_named_acl_input_preserves_original_recipient_forms_and_defaults() {
    let recipients = [
        r#""PUBLIC""#,
        r#""reader""#,
        r#""한글🙂""#,
        r#"{"kind":"public"}"#,
        r#"{"kind":"public","name":null}"#,
        r#"{"kind":"role","name":"PUBLIC"}"#,
        r#"{"name":"reader","kind":"role"}"#,
        r#"["role","reader"]"#,
        r#"["public"]"#,
        r#"["public",null]"#,
        r#"{"kind":"role"}"#,
        r#"{"kind":"public","name":"reader"}"#,
        r#"{"kind":"role","name":"reader","extra":0}"#,
        r#"{"kind":"role","kind":"public"}"#,
        r#"{"kind":"role","name":null}"#,
        "null",
        "42",
    ];
    for recipient in recipients {
        for entry in [
            format!(r#"{{"role":{recipient}}}"#),
            format!(
                r#"{{"role":{recipient},"grantor":"owner","privileges":{{"select":true}},"grant_options":{{"select":true}}}}"#
            ),
            format!(r"[{recipient},null,{{}},{{}}]"),
        ] {
            let json = format!("[{entry}]");
            let expected: std::result::Result<Option<Vec<TableAclEntry>>, _> =
                serde_json::from_str(&json);
            let memory = MemoryBudget::new(65536);
            let cancellation = CancellationToken::new();
            let actual = legacy_entries(json.as_bytes(), &memory, &cancellation);
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{json}"),
                (Err(_), Err(_)) => (),
                (expected, actual) => panic!("{json}: expected {expected:?}, actual {actual:?}"),
            }
            assert_eq!(memory.used(), 0);
        }
    }
    for text in [
        "null",
        "[]",
        r#"[{"role":"reader","ignored":["\ud800",1e9999]}]"#,
    ] {
        let expected: Option<Vec<TableAclEntry>> = serde_json::from_str(text).unwrap();
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        let actual = legacy_entries(text.as_bytes(), &memory, &cancellation).unwrap();
        assert_eq!(*actual, expected);
        drop(actual);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn durable_named_column_acl_input_keeps_nodes_names_and_replacement_leases() {
    let repeated = "recipient한🙂".repeat(1024);
    let grantor = "grantor".repeat(1024);
    let entry = serde_json::to_string(&TableAclEntry {
        role: AclGrantee::Role(repeated),
        grantor: Some(grantor),
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    })
    .unwrap();
    let json =
        format!(r#"{{"column":[{entry}],"other":[{entry}],"column":[{{"role":"PUBLIC"}}]}}"#);
    let expected: BTreeMap<String, Vec<TableAclEntry>> = serde_json::from_str(&json).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut actual = None;
    let measured = allocation_counter::measure(|| {
        actual = Some(legacy_column_entries(json.as_bytes(), &memory, &cancellation).unwrap());
    });
    let actual = actual.unwrap();
    assert_eq!(
        serde_json::to_value(&*actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(measured.bytes_current, memory.used() as i64);
    let retained = actual
        .iter()
        .map(|(name, entries)| {
            Columns::entry_bytes() + name.capacity() + acl_bytes(entries, &cancellation).unwrap()
        })
        .sum::<usize>();
    assert_eq!(actual.reserved_bytes(), retained);
    drop(actual);
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_named_acl_input_retires_partial_fields_and_denied_allocations() {
    let json = r#"[{"role":{"kind":"role","name":"한🙂"},"grantor":"owner","privileges":{"select":true}}]"#;
    for end in 0..json.len() {
        let memory = MemoryBudget::new(65536);
        let sibling = memory.reserve(19).unwrap();
        let cancellation = CancellationToken::new();
        assert!(legacy_entries(&json.as_bytes()[..end], &memory, &cancellation).is_err());
        assert_eq!(memory.used(), sibling.bytes());
    }
    let json = format!(
        r#"{{"field":[{{"role":"{}","grantor":"{}"}}]}}"#,
        "r".repeat(8192),
        "g".repeat(8192)
    );
    for available in [1, 31, 255, 4096, 8192, 16384] {
        let memory = MemoryBudget::new(available + 19);
        let sibling = memory.reserve(19).unwrap();
        let cancellation = CancellationToken::new();
        assert!(legacy_column_entries(json.as_bytes(), &memory, &cancellation).is_err());
        assert_eq!(memory.used(), sibling.bytes());
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        legacy_entries(b"null", &memory, &cancellation),
        Err(crate::json::JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_named_acl_input_preserves_named_security_wire_after_node_adoption() {
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Original {
        role_owner: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acl: Option<Vec<TableAclEntry>>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        column_acls: BTreeMap<String, Vec<TableAclEntry>>,
    }
    let json = r#"{"role_owner":"owner","acl":null,"column_acls":{"field":[{"role":"PUBLIC"}]}}"#;
    let security: crate::catalog_acl::LegacyRelationSecurity = serde_json::from_str(json).unwrap();
    let expected: Original = serde_json::from_str(json).unwrap();
    assert_eq!(
        serde_json::to_value(security).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
