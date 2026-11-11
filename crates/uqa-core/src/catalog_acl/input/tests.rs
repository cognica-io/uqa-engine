//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

fn entry() -> Entry {
    Entry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    }
}

#[test]
fn durable_relation_acl_input_preserves_existing_struct_and_map_shapes() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    let entry = serde_json::to_string(&entry()).unwrap();
    let unknown = format!(
        r#"{{"ignored":{}[1e9999,"\ud800"]{},"grant_options":{{}},"privileges":{{"select":true}},"grantor":{role},"role":null}}"#,
        "[".repeat(256),
        "]".repeat(256),
    );
    let shapes = [
        "null".to_owned(),
        "[]".to_owned(),
        format!("[{entry}]"),
        format!("[[null,{role},[],[true,false,true]]]"),
        format!("[{unknown}]"),
        format!(
            r#"[{{"\u0072ole":null,"grantor":{role},"privileges":{{"maintain":true}},"grant_options":{{}}}}]"#
        ),
    ];
    for json in shapes {
        let expected: Option<Vec<Entry>> = serde_json::from_str(&json).unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let restored = entries(json.as_bytes(), &memory, &cancellation).unwrap();
        assert_eq!(*restored, expected);
        drop(restored);
        assert_eq!(memory.used(), 0);
        if expected.is_some() {
            let text = format!(r#"{{"한\u0000글":{json},"a":[],"한\u0000글":[]}}"#);
            let expected: BTreeMap<String, Vec<Entry>> = serde_json::from_str(&text).unwrap();
            let restored = column_entries(text.as_bytes(), &memory, &cancellation).unwrap();
            assert!(restored.iter().eq(expected.iter()));
            assert_eq!(
                serde_json::to_vec(&*restored).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            drop(restored);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn durable_role_input_preserves_integer_boundaries_and_original_validity_checks() {
    let ids = serde_json::to_string(&[0_u8; 16]).unwrap();
    let negative_zero = format!(
        "[-0,{}]",
        std::iter::repeat_n("0", 15).collect::<Vec<_>>().join(",")
    );
    let shapes = [
        serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap(),
        format!(r#"{{"oid":-9223372036854775808,"object_id":{ids}}}"#),
        format!(r#"{{"object_id":{ids},"oid":9223372036854775807}}"#),
        format!("[0,{ids}]"),
        format!(r#"{{"oid":1,"object_id":{ids},"unknown":[1e9999,"\ud800"]}}"#),
    ];
    for json in shapes {
        let expected: RoleIdentity = serde_json::from_str(&json).unwrap();
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        assert_eq!(
            role_identity(json.as_bytes(), &memory, &cancellation).unwrap(),
            expected
        );
        assert_eq!(memory.used(), 0);
    }
    for json in [
        "null".to_owned(),
        "[]".to_owned(),
        "{}".to_owned(),
        format!(r#"{{"object_id":{negative_zero},"oid":1}}"#),
        format!("[-0,{ids}]"),
        format!("[1,{ids},0]"),
        format!(r#"{{"oid":1.0,"object_id":{ids}}}"#),
        format!(r#"{{"oid":9223372036854775808,"object_id":{ids}}}"#),
        format!(
            r#"{{"oid":1,"object_id":[256,{}]}}"#,
            std::iter::repeat_n("0", 15).collect::<Vec<_>>().join(",")
        ),
        format!(
            r#"{{"oid":1,"object_id":[-1,{}]}}"#,
            std::iter::repeat_n("0", 15).collect::<Vec<_>>().join(",")
        ),
        format!(r#"{{"oid":1,"object_id":{ids}}} 0"#),
    ] {
        assert!(
            serde_json::from_str::<RoleIdentity>(&json).is_err(),
            "{json}"
        );
        let memory = MemoryBudget::new(65536);
        assert!(
            role_identity(json.as_bytes(), &memory, &CancellationToken::new()).is_err(),
            "{json}"
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn durable_relation_acl_input_keeps_duplicate_and_missing_field_error_order() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for (text, expected) in [
        (
            r#"[{"role":null,"role":!}]"#.to_owned(),
            "duplicate field `role`",
        ),
        ("[{}] 0".to_owned(), "missing field `role`"),
        (
            format!(
                r#"[{{"role":null,"grantor":{role},"privileges":{{"select":true,"select":!}},"grant_options":{{}}}}]"#
            ),
            "duplicate field `select`",
        ),
    ] {
        assert!(serde_json::from_str::<Vec<Entry>>(&text)
            .unwrap_err()
            .to_string()
            .contains(expected));
        let memory = MemoryBudget::new(65536);
        let error = entries(text.as_bytes(), &memory, &CancellationToken::new()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(memory.used(), 0);
    }
    let entry = serde_json::to_string(&entry()).unwrap();
    for text in [
        format!("[{entry},null]"),
        format!("[{entry},[]]"),
        format!(
            r#"[{{"role":null,"grantor":{role},"privileges":{{"select":1}},"grant_options":{{}}}}]"#
        ),
        format!("[[null,{role},{{}}]]"),
        format!("[[null,{role},{{}},{{}},0]]"),
    ] {
        assert!(serde_json::from_str::<Vec<Entry>>(&text).is_err());
        let memory = MemoryBudget::new(65536);
        assert!(entries(text.as_bytes(), &memory, &CancellationToken::new()).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn durable_column_acl_input_retains_exact_buffers_and_releases_replaced_paths() {
    let mut paths = vec![entry(); 257];
    paths[0].role = Some(RoleIdentity::BOOTSTRAP);
    let paths = serde_json::to_string(&paths).unwrap();
    let text = format!(
        r#"{{"z":{paths},"wide{}":{paths},"z":[],"a":{paths}}}"#,
        "한글".repeat(4096)
    );
    let expected: BTreeMap<String, Vec<Entry>> = serde_json::from_str(&text).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut output = None;
    let observed = allocation_counter::measure(|| {
        output = Some(column_entries(text.as_bytes(), &memory, &cancellation).unwrap());
    });
    let output = output.unwrap();
    assert!(output.iter().eq(expected.iter()));
    assert_eq!(
        observed.bytes_current,
        i64::try_from(memory.used()).unwrap(),
        "{observed:?}"
    );
    assert_eq!(output.reserved_bytes(), memory.used());
    drop(output);
    assert_eq!(memory.used(), 0);
    let text = format!(r#"{{"{}":[]}}"#, "x".repeat(4 << 20));
    let limited = MemoryBudget::new(128);
    let observed = allocation_counter::measure(|| {
        assert!(matches!(
            column_entries(text.as_bytes(), &limited, &cancellation),
            Err(JsonReadError::Memory(_))
        ));
    });
    assert!(observed.bytes_total < 4096, "{observed:?}");
    assert_eq!(limited.used(), 0);
}

#[test]
fn durable_column_acl_input_retires_truncation_cancellation_and_partial_admission() {
    let paths = serde_json::to_string(&vec![entry(); 8]).unwrap();
    let text = format!(r#"{{"first":{paths},"second":{paths}}}"#);
    for length in 0..text.len() {
        let memory = MemoryBudget::new(65536);
        assert!(column_entries(
            &text.as_bytes()[..length],
            &memory,
            &CancellationToken::new()
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    for available in [0, 1, 16, 32, 64, 128, 256, 512, 1024] {
        let memory = MemoryBudget::new(65536);
        let sibling = memory.reserve(17).unwrap();
        let occupied = memory.reserve(memory.available() - available).unwrap();
        assert!(matches!(
            column_entries(text.as_bytes(), &memory, &CancellationToken::new()),
            Err(JsonReadError::Memory(_))
        ));
        assert_eq!(memory.used(), sibling.bytes() + occupied.bytes());
        drop(occupied);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        column_entries(text.as_bytes(), &memory, &cancellation),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
    for text in [
        br#"{"a":null}"#.as_slice(),
        br#"{"\ud800":[]}"#,
        br#"{"a":[]} true"#,
    ] {
        assert!(serde_json::from_slice::<BTreeMap<String, Vec<Entry>>>(text).is_err());
        assert!(column_entries(text, &memory, &CancellationToken::new()).is_err());
        assert_eq!(memory.used(), 0);
    }
}
