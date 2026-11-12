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
        privileges: SchemaPrivileges::ALL,
        grant_options: SchemaPrivileges::default(),
    })
    .unwrap()
}

#[test]
fn schema_acl_input_preserves_direct_bound_and_named_struct_shapes() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for privileges in [
        "{}",
        "[]",
        "[true]",
        "[true,false]",
        r#"{"create":true}"#,
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
fn schema_acl_projection_preserves_last_fields_and_original_numeric_classes() {
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    for text in [
        format!(r#"[{{"role":false,"role":null,"grantor":{role},"privileges":{{"usage":false,"usage":true}},"grant_options":{{}}}}]"#),
        r#"[{"role":false,"role":"reader","grantor":true,"grantor":"owner","privileges":null,"privileges":{"create":true}}]"#.into(),
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

fn tuple(
    bytes: &[u8],
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<Budgeted<SchemaTupleIdentity>> {
    Ok(Budgeted::new(
        projected_tuple_identity(bytes, memory, token)?,
        memory.empty_reservation(),
    ))
}

#[test]
fn schema_tuple_projection_preserves_original_fields_and_defers_validity() {
    let bytes = serde_json::to_string(&SchemaTupleIdentity::initial(2200)).unwrap();
    for text in [
        bytes.clone(),
        bytes.replace("2200", "0"),
        bytes.replace("2200", "-0"),
        bytes.replace("2200", "1.0"),
        bytes.replace("\"oid\":2200", "\"oid\":false,\"oid\":2200"),
        bytes.replace("\"oid\":2200", "\"oid\":2200,\"unknown\":0"),
        bytes.replace("\"revision\"", "\"missing\""),
        "null".into(),
    ] {
        equivalent(text.as_bytes(), tuple, true);
    }
    for end in 0..bytes.len() {
        equivalent(&bytes.as_bytes()[..end], tuple, true);
    }
}

fn bound_row() -> String {
    let mut row = BoundSchemaRow::bootstrap("public");
    row.tuple = Some(SchemaTupleIdentity::initial(2200));
    serde_json::to_string(&row).unwrap()
}

#[test]
fn schema_row_input_preserves_projected_bound_fields_and_legacy_defaults() {
    let bound = bound_row();
    let row: serde_json::Value = serde_json::from_str(&bound).unwrap();
    let positional = serde_json::to_string(&serde_json::json!([
        row["name"],
        row["tuple"],
        row["role_owner"],
        row["acl"]
    ]))
    .unwrap();
    for text in [
        bound.clone(),
        positional,
        bound.replace("\"name\":\"public\"", "\"name\":false,\"name\":\"public\""),
        bound.replace("\"name\":\"public\"", "\"name\":\"public\",\"unknown\":[]"),
        r#"{"name":"public"}"#.into(),
        r#"{"name":"public","role_owner":"owner","acl":null}"#.into(),
        r#"{"name":"public","role_owner":null}"#.into(),
        r#"["schema"]"#.into(),
        r#"["schema","owner"]"#.into(),
        r#"["schema","owner",[]]"#.into(),
        "[]".into(),
        "null".into(),
        "{}".into(),
    ] {
        equivalent(text.as_bytes(), projected_bound_row, true);
        equivalent(text.as_bytes(), projected_legacy_row, true);
    }
}

#[test]
fn schema_row_input_rejects_original_malformed_names_and_nested_fields() {
    malformed(&bound_row(), projected_bound_row, true);
    malformed(
        r#"{"name":"schema한글","role_owner":"owner","acl":[{"role":"PUBLIC"}]}"#,
        projected_legacy_row,
        true,
    );
}

#[test]
fn schema_row_input_retains_names_authority_and_default_owner_allocations() {
    retention(&bound_row(), projected_bound_row);
    retention(
        r#"{"name":"schema한글","acl":[{"role":"PUBLIC"},{"role":"reader","grantor":"owner"}]}"#,
        projected_legacy_row,
    );
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
fn schema_acl_input_rejects_original_malformed_fields_and_truncated_prefixes() {
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
fn schema_acl_input_accounts_for_native_buffers_and_retires_denied_prefixes() {
    let bound = format!("[{},{}]", bound_entry(), bound_entry());
    let named = r#"[{"role":"reader한글","grantor":"owner"},{"role":"PUBLIC"}]"#;
    retention(&bound, entries);
    retention(&bound, projected_entries);
    retention(named, legacy_entries);
    retention(named, projected_legacy_entries);
}

#[test]
fn schema_acl_cancellation_retires_active_paths_and_preserves_completed_output() {
    let text = format!(
        r#"[{{"role":"{}"}},{{"role":"PUBLIC"}}]"#,
        "reader한글".repeat(1024)
    );
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let completed = legacy_entries(text.as_bytes(), &memory, &token).unwrap();
    let retained = memory.used();
    let mut tokens = reader(text.as_bytes(), &memory, &token);
    let mut input = Input::new(&mut tokens, &memory, &token);
    let first = input.next().unwrap();
    let actual = input.acl_with(first, |input, first| {
        let entry = input.legacy_fields::<SchemaPrivileges>(first)?;
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
fn legacy_schema_construction_admits_original_defaults_before_allocating() {
    use crate::{catalog_schema::SchemaRow, memory::ProductionControl};
    for name in [
        String::new(),
        "public".into(),
        "ordinary".into(),
        "한글".repeat(4096),
    ] {
        let expected = SchemaRow::legacy(name.clone());
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &invoking);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(SchemaRow::legacy_with_control(&name, &control).unwrap());
        });
        assert_eq!(**output.as_ref().unwrap(), expected);
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            memory.used()
        );
        assert_eq!(
            usize::try_from(allocations.bytes_max).unwrap(),
            memory.peak()
        );
        let required = memory.peak();
        drop(output);
        assert_eq!(memory.used(), 0);
        let denied = MemoryBudget::new(required - 1);
        assert!(SchemaRow::legacy_with_control(
            &name,
            &ProductionControl::new(&denied, &token, &invoking)
        )
        .is_err());
        assert_eq!(denied.used(), 0);
        for original in [true, false] {
            let token = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original {
                token.cancel();
            } else {
                invoking.cancel();
            }
            let allocations = allocation_counter::measure(|| {
                assert!(SchemaRow::legacy_with_control(
                    &name,
                    &ProductionControl::new(&memory, &token, &invoking)
                )
                .is_err());
            });
            assert_eq!(allocations.bytes_total, 0);
        }
    }
}
