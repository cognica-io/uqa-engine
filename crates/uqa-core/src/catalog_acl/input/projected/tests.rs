//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog_acl::{AclGrantee, TablePrivileges};
use crate::json::JsonReadError;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

#[test]
fn projected_column_failure_destroys_native_values_before_returning_their_allowance() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Observation {
        minimum: AtomicUsize,
        dropped: AtomicUsize,
    }
    struct Probe {
        memory: MemoryBudget,
        observed: Arc<Observation>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            self.observed
                .minimum
                .fetch_min(self.memory.used(), Ordering::SeqCst);
            self.observed.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let observed = Arc::new(Observation {
        minimum: AtomicUsize::new(usize::MAX),
        dropped: AtomicUsize::new(0),
    });
    let mut count = 0;
    let result = columns(br#"{"a":0,"b":0}"#, &memory, &token, |_, _| {
        count += 1;
        if count == 2 {
            return Err(invalid("later column rejection"));
        }
        let mut values = crate::memory::BudgetedVec::new(&memory);
        for _ in 0..1024 {
            values.push(Probe {
                memory: memory.clone(),
                observed: observed.clone(),
            })?;
        }
        let (values, lease) = values.into_parts();
        Ok(Budgeted::new(values, lease))
    });
    assert!(result.is_err());
    assert_eq!(observed.dropped.load(Ordering::SeqCst), 1024);
    assert!(observed.minimum.load(Ordering::SeqCst) >= 1024 * size_of::<Probe>());
    assert_eq!(memory.used(), 0);
}

fn reference<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    serde_json::from_str::<serde_json::Value>(text).and_then(serde_json::from_value)
}

fn bound_entry() -> String {
    serde_json::to_string(&Entry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    })
    .unwrap()
}

#[test]
fn projected_authority_roles_preserve_last_fields_and_value_integer_classification() {
    let zeros = serde_json::to_string(&[0_u8; 16]).unwrap();
    for number in [
        "-0",
        "0",
        "-1",
        "255",
        "256",
        "1.0",
        "1e0",
        "1e9999",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "-9223372036854775808",
        "-9223372036854775809",
    ] {
        for text in [
            format!(r#"{{"oid":{number},"object_id":{zeros}}}"#),
            format!(r#"{{"oid":false,"oid":{number},"object_id":{zeros}}}"#),
            format!(
                r#"{{"oid":1,"object_id":[{number},{}]}}"#,
                std::iter::repeat_n("0", 15).collect::<Vec<_>>().join(",")
            ),
            format!("[{number},{zeros}]"),
        ] {
            let expected = reference::<RoleIdentity>(&text);
            let memory = MemoryBudget::new(65536);
            let actual = role_identity(text.as_bytes(), &memory, &CancellationToken::new());
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{text}"),
                (Err(_), Err(_)) => (),
                (expected, actual) => panic!("{text}: {expected:?}, {actual:?}"),
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn projected_bound_authority_preserves_recursive_collisions_and_sorted_validation() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for text in [
        "null".to_owned(),
        "[]".to_owned(),
        format!("[{}]", bound_entry()),
        format!(
            r#"[{{"role":42,"role":null,"grantor":{role},"privileges":{{"select":false,"select":true}},"grant_options":{{}}}}]"#
        ),
        format!(r"[[null,{role},[true],{{}}]]"),
        format!(
            r#"[{{"role":null,"grantor":{role},"privileges":{{}},"grant_options":{{}},"ignored":1e9999}}]"#
        ),
        format!(
            r#"[{{"role":null,"grantor":{role},"privileges":{{}},"grant_options":{{}},"ignored":"\ud800"}}]"#
        ),
        "[{}]".to_owned(),
        format!(
            r#"[{{"role":null,"grantor":{role},"privileges":{{"select":true,"select":null}},"grant_options":{{}}}}]"#
        ),
    ] {
        let expected = reference::<Option<Vec<Entry>>>(&text);
        let memory = MemoryBudget::new(65536);
        let actual = entries(text.as_bytes(), &memory, &CancellationToken::new());
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{text}"),
            (Err(_), Err(_)) => (),
            (expected, actual) => panic!("{text}: {expected:?}, {actual:?}"),
        }
        assert_eq!(memory.used(), 0);
    }
    let text = r#"[{"role":{"object_id":[]},"grantor":{},"privileges":{},"grant_options":{}}]"#;
    assert!(reference::<Vec<Entry>>(text)
        .unwrap_err()
        .to_string()
        .contains("missing field `oid`"));
    let memory = MemoryBudget::new(65536);
    let actual = entries(text.as_bytes(), &memory, &CancellationToken::new()).unwrap_err();
    assert!(
        actual.to_string().contains("missing field `oid`"),
        "{actual}"
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn projected_named_authority_preserves_recipient_tags_and_overwritten_values() {
    for recipient in [
        r#""PUBLIC""#,
        r#""reader""#,
        r#"{"kind":"role","name":"PUBLIC"}"#,
        r#"{"kind":"unknown","kind":"public"}"#,
        r#"{"kind":"role","name":false,"name":"reader"}"#,
        r#"{"kind":"public","name":"reader","name":null}"#,
        r#"{"kind":"role","name":"reader","extra":null}"#,
        r#"["public",null]"#,
        r#"["public"]"#,
        r#"["role","reader"]"#,
        "null",
        "42",
    ] {
        for text in [
            format!(r#"[{{"role":{recipient}}}]"#),
            format!(
                r#"[{{"role":false,"role":{recipient},"grantor":false,"grantor":null,"privileges":{{"update":false,"update":true}}}}]"#
            ),
            format!(r"[[{recipient},null,{{}},{{}}]]"),
        ] {
            let expected = reference::<Option<Vec<TableAclEntry>>>(&text);
            let memory = MemoryBudget::new(65536);
            let actual = legacy_entries(text.as_bytes(), &memory, &CancellationToken::new());
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{text}"),
                (Err(_), Err(_)) => (),
                (expected, actual) => panic!("{text}: {expected:?}, {actual:?}"),
            }
            assert_eq!(memory.used(), 0);
        }
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    let actual = legacy_entries(
        br#"[{"role":{"kind":"role","name":"PUBLIC"}}]"#,
        &memory,
        &cancellation,
    )
    .unwrap();
    assert_eq!(
        actual.as_ref().unwrap()[0].role,
        AclGrantee::Role("PUBLIC".into())
    );
}

#[test]
fn projected_column_authority_admits_only_final_paths_and_keeps_exact_live_owners() {
    let large = "x".repeat(4 << 20);
    let texts = [
        format!(
            r#"{{"z":false,"a":[{}],"z":[],"\u0061":[{}]}}"#,
            bound_entry(),
            bound_entry()
        ),
        format!(r#"{{"z":"{large}","a":[],"z":[{}]}}"#, bound_entry()),
    ];
    for text in texts {
        let expected = reference::<BTreeMap<String, Vec<Entry>>>(&text).unwrap();
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        let mut actual = None;
        let measured = allocation_counter::measure(|| {
            actual = Some(column_entries(text.as_bytes(), &memory, &cancellation).unwrap());
        });
        let actual = actual.unwrap();
        assert!(actual.iter().eq(expected.iter()));
        assert_eq!(measured.bytes_current, memory.used() as i64);
        assert!(measured.bytes_max < 65536, "{measured:?}");
        assert_eq!(actual.reserved_bytes(), memory.used());
        drop(actual);
        assert_eq!(memory.used(), 0);
    }
    let text = r#"{"field":[{"role":false}],"other":[{"role":"reader","grantor":"owner"}],"field":[{"role":"PUBLIC"}]}"#;
    let expected = reference::<BTreeMap<String, Vec<TableAclEntry>>>(text).unwrap();
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    let mut actual = None;
    let measured = allocation_counter::measure(|| {
        actual = Some(legacy_column_entries(text.as_bytes(), &memory, &cancellation).unwrap());
    });
    let actual = actual.unwrap();
    assert!(actual.iter().eq(expected.iter()));
    assert_eq!(measured.bytes_current, memory.used() as i64);
    assert_eq!(actual.reserved_bytes(), memory.used());
    drop(actual);
    assert_eq!(memory.used(), 0);
}

#[test]
fn projected_authority_retires_partial_input_and_denied_names_without_value_copies() {
    let text = r#"{"first":[{"role":"reader","grantor":"owner"}],"second":[{"role":{"kind":"role","name":"한🙂"}}]}"#;
    for end in 0..text.len() {
        let memory = MemoryBudget::new(65536);
        let sibling = memory.reserve(19).unwrap();
        assert!(
            legacy_column_entries(&text.as_bytes()[..end], &memory, &CancellationToken::new())
                .is_err()
        );
        assert_eq!(memory.used(), sibling.bytes());
    }
    let text = format!(r#"{{"field":[{{"role":"{}"}}]}}"#, "x".repeat(1 << 20));
    for limit in [1, 31, 255, 4096, 65536] {
        let memory = MemoryBudget::new(limit + 19);
        let sibling = memory.reserve(19).unwrap();
        let cancellation = CancellationToken::new();
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                legacy_column_entries(text.as_bytes(), &memory, &cancellation),
                Err(JsonReadError::Memory(_))
            ));
        });
        assert!(measured.bytes_max < 65536);
        assert_eq!(memory.used(), sibling.bytes());
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        entries(b"null", &memory, &cancellation),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
