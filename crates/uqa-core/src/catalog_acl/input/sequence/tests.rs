//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog_role::RoleIdentity, json::JsonReadError};
use serde::de::DeserializeOwned;

type Read<T> = fn(&[u8], &MemoryBudget, &CancellationToken) -> Result<Budgeted<T>>;

fn equivalent<T: DeserializeOwned + std::fmt::Debug + PartialEq>(
    bytes: &[u8],
    read: Read<T>,
    projected: bool,
) {
    let expected = if projected {
        serde_json::from_slice::<serde_json::Value>(bytes).and_then(serde_json::from_value)
    } else {
        serde_json::from_slice::<T>(bytes)
    };
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    match (expected, read(bytes, &memory, &token)) {
        (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{bytes:?}"),
        (Err(_), Err(_)) => (),
        (expected, actual) => panic!("{bytes:?}: expected {expected:?}, actual {actual:?}"),
    }
    assert_eq!(memory.used(), 0);
}

fn bound_entry() -> String {
    serde_json::to_string(&Entry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    })
    .unwrap()
}

#[test]
fn sequence_acl_input_preserves_direct_bound_and_named_struct_shapes() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for privileges in [
        "{}",
        "[]",
        "[true]",
        "[true,false]",
        "[true,false,true]",
        "[true,false,true,false]",
        r#"{"select":true}"#,
        "null",
        "42",
    ] {
        for text in [
            format!(
                r#"[{{"role":null,"grantor":{role},"privileges":{privileges},"grant_options":{{}}}}]"#
            ),
            format!(r"[[null,{role},{privileges},{{}}]]"),
            format!(r#"[{{"role":"PUBLIC","privileges":{privileges}}}]"#),
            format!(r#"[["reader",null,{privileges},{{}}]]"#),
        ] {
            equivalent(text.as_bytes(), entries, false);
            equivalent(text.as_bytes(), legacy_entries, false);
        }
    }
    for text in [
        "null".to_owned(),
        "[]".to_owned(),
        format!("[{}]", bound_entry()),
        r#"[{"role":{"kind":"role","name":"PUBLIC"},"grantor":"owner한글"}]"#.into(),
        r#"[{"role":"PUBLIC","ignored":["\ud800",1e9999]}]"#.into(),
        r#"[{"role":{"kind":"public","name":null}}]"#.into(),
        r#"[{"role":{"kind":"public","name":"invalid"}}]"#.into(),
        r#"[{"role":"reader","role":"PUBLIC"}]"#.into(),
        r#"[{"grantor":"owner"}]"#.into(),
        r#"[{"role":null,"grantor":null,"privileges":{},"grant_options":{}}]"#.into(),
    ] {
        equivalent(text.as_bytes(), entries, false);
        equivalent(text.as_bytes(), legacy_entries, false);
    }
}

#[test]
fn sequence_acl_projection_preserves_last_fields_and_original_numeric_classes() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for text in [
        format!(r#"[{{"role":false,"role":null,"grantor":{role},"privileges":{{"usage":false,"usage":true}},"grant_options":{{}}}}]"#),
        r#"[{"role":false,"role":"reader","grantor":true,"grantor":"owner","privileges":null,"privileges":{"select":true}}]"#.into(),
        r#"[{"role":{"kind":"public","kind":"role","name":"reader"}}]"#.into(),
        r#"[{"role":"reader","unknown":"\ud800"}]"#.into(),
        r#"[{"role":"reader","unknown":1e9999}]"#.into(),
        r#"[{"role":"reader","unknown":{"ignored":false,"ignored":true}}]"#.into(),
        "null".into(),
        "[]".into(),
    ] {
        equivalent(text.as_bytes(), projected_entries, true);
        equivalent(text.as_bytes(), projected_legacy_entries, true);
    }
    for number in ["-0", "1.0", "1e0", "9223372036854775808", "-1", "1e9999"] {
        let role = role.replace("\"oid\":10", &format!("\"oid\":{number}"));
        let text =
            format!(r#"[{{"role":null,"grantor":{role},"privileges":{{}},"grant_options":{{}}}}]"#);
        equivalent(text.as_bytes(), projected_entries, true);
    }
}

fn malformed<T: DeserializeOwned + std::fmt::Debug + PartialEq>(
    text: &str,
    read: Read<T>,
    projected: bool,
) {
    for end in 0..text.len() {
        equivalent(&text.as_bytes()[..end], read, projected);
    }
    for position in 0..text.len() {
        for replacement in [b'0', b'f', b'"', b'[', b'}', 0xff] {
            let mut bytes = text.as_bytes().to_vec();
            bytes[position] = replacement;
            equivalent(&bytes, read, projected);
        }
    }
}

#[test]
fn sequence_acl_input_rejects_original_malformed_fields_and_truncated_prefixes() {
    let bound = format!("[{}]", bound_entry());
    let named = r#"[{"role":{"kind":"role","name":"reader한글"},"grantor":"owner","privileges":{"usage":true},"grant_options":{}}]"#;
    malformed(&bound, entries, false);
    malformed(&bound, projected_entries, true);
    malformed(named, legacy_entries, false);
    malformed(named, projected_legacy_entries, true);
}

fn retention<T: std::fmt::Debug>(text: &str, read: Read<T>) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read(text.as_bytes(), &memory, &token).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocations.bytes_max).unwrap() <= memory.peak());
    assert_eq!(output.as_ref().unwrap().reserved_bytes(), memory.used());
    let peak = memory.peak();
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(37).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        assert!(read(text.as_bytes(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn sequence_acl_input_accounts_for_native_buffers_and_retires_denied_prefixes() {
    let bound = format!("[{},{}]", bound_entry(), bound_entry());
    let named = r#"[{"role":"reader한글","grantor":"owner"},{"role":"PUBLIC"}]"#;
    retention(&bound, entries);
    retention(&bound, projected_entries);
    retention(named, legacy_entries);
    retention(named, projected_legacy_entries);
}

#[test]
fn sequence_acl_cancellation_retires_active_paths_and_preserves_completed_output() {
    let text = format!(
        r#"[{{"role":"{}"}},{{"role":"PUBLIC"}}]"#,
        "reader한글".repeat(1024)
    );
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let completed = legacy_entries(text.as_bytes(), &memory, &token).unwrap();
    let retained = memory.used();
    let mut tokens = crate::catalog_acl::input::reader(text.as_bytes(), &memory, &token);
    let mut input = Input::new(&mut tokens, &memory, &token);
    let first = input.next().unwrap();
    let actual = input.acl_with(first, |input, first| {
        let entry = input.legacy_fields::<SequencePrivileges>(first)?;
        assert!(memory.used() > retained + 4096);
        token.cancel();
        Ok(entry)
    });
    assert!(matches!(actual, Err(JsonReadError::Cancelled(_))));
    drop(tokens);
    assert_eq!(memory.used(), retained);
    for read in [legacy_entries, projected_legacy_entries] {
        let allocations = allocation_counter::measure(|| {
            assert!(matches!(
                read(text.as_bytes(), &memory, &token),
                Err(JsonReadError::Cancelled(_))
            ));
        });
        assert_eq!(allocations.bytes_total, 0);
    }
    assert_eq!(completed.as_ref().unwrap().len(), 2);
    drop(completed);
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_owner_prefix_preserves_fixed_identity_and_dependency_shapes() {
    use crate::catalog_sequence::SequenceOwner;
    let id = "[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]";
    for dependency in [
        "\"automatic\"",
        "\"internal\"",
        "null",
        "{}",
        "{\"internal\":null}",
        "{\"automatic\":false}",
    ] {
        let text = format!(
            r#"{{"table_object_id":{id},"column_object_id":{id},"dependency":{dependency}}}"#
        );
        let expected = serde_json::from_str::<Option<SequenceOwner>>(&text);
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        match (
            expected,
            owner_prefix(text.as_bytes(), &memory, &cancellation),
        ) {
            (Ok(expected), Ok(actual)) => assert_eq!(actual, expected),
            (Err(_), Err(_)) => (),
            (expected, actual) => panic!("{text}: {expected:?} / {actual:?}"),
        }
        assert_eq!(memory.used(), 0);
    }
}
