//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde::de::DeserializeOwned;
use uqa_core::{memory::MemoryBudget, CancellationToken};

const ROLE: &str =
    r#"{"oid":41,"name":"reader_역할","attributes":["Login","Inherit"],"connection_limit":-1}"#;

fn membership() -> String {
    let binding = r#"{"name":"bound_역할","oid":41,"object_id":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#;
    format!(
        r#"{{"oid":57,"role":{binding},"member":{binding},"grantor":{binding},"admin_option":false,"inherit_option":true,"set_option":true}}"#
    )
}

type Read<T> = for<'a> fn(&str, &ProductionControl<'a>) -> Result<Produced<T>, SQLError>;

fn equivalent<T: DeserializeOwned + PartialEq + std::fmt::Debug>(text: &str, read: Read<T>) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let expected = serde_json::from_str::<T>(text);
    let actual = read(text, &ProductionControl::new(&memory, &token, &token));
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert_eq!(*actual, expected, "{text}"),
        (Err(_), Err(_)) => (),
        (actual, expected) => panic!("stored record differs: {text}, {actual:?}, {expected:?}"),
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_role_json_preserves_original_fields_defaults_and_container_forms() {
    let modern = serde_json::to_string(&RoleDefinition::bootstrap()).unwrap();
    let sequence = format!(r#"[10,[{}],1,"uqa",["Login"],-1]"#, "0,".repeat(15) + "0");
    for text in [
        ROLE.to_owned(),
        modern,
        sequence,
        ROLE.replace("41", "-1").replace("-1}", "-27}"),
        ROLE.replace("\"Login\"", "{\"Login\":null},\"Login\""),
        ROLE.replace("\"name\"", "\"ignored\":{\"nested\":[1,true,{}]},\"name\""),
    ] {
        equivalent(&text, RoleDefinition::from_stored_json_with_control);
    }
}

#[test]
fn stored_membership_json_preserves_original_bound_incarnations() {
    let text = membership();
    for candidate in [
        text.clone(),
        text.replace("41", "0"),
        text.replace("bound_역할", "renamed_роль"),
        text.replace(
            "\"admin_option\":false",
            "\"ignored\":[1,2],\"admin_option\":false",
        ),
    ] {
        equivalent(&candidate, RoleMembership::from_stored_json_with_control);
    }
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let fields: Vec<_> = [
        "oid",
        "role",
        "member",
        "grantor",
        "admin_option",
        "inherit_option",
        "set_option",
    ]
    .iter()
    .map(|field| value[*field].clone())
    .collect();
    equivalent(
        &serde_json::to_string(&fields).unwrap(),
        RoleMembership::from_stored_json_with_control,
    );
}

fn mutations<T: DeserializeOwned + PartialEq + std::fmt::Debug>(text: &str, read: Read<T>) {
    for end in 0..text.len() {
        if text.is_char_boundary(end) {
            equivalent(&text[..end], read);
        }
    }
    for position in 0..text.len() {
        for replacement in [b'0', b'f', b'"', b'[', b'}'] {
            let mut bytes = text.as_bytes().to_vec();
            bytes[position] = replacement;
            if let Ok(candidate) = std::str::from_utf8(&bytes) {
                equivalent(candidate, read);
            }
        }
    }
}

#[test]
fn stored_role_records_reject_original_malformed_fields_and_truncated_prefixes() {
    mutations(ROLE, RoleDefinition::from_stored_json_with_control);
    mutations(&membership(), RoleMembership::from_stored_json_with_control);
    for text in [
        ROLE.replace("\"oid\":41", "\"oid\":41,\"oid\":42"),
        ROLE.replace("\"Login\"", "\"Unknown\""),
        ROLE.replace("\"name\":", "\"removed_name\":"),
    ] {
        equivalent(&text, RoleDefinition::from_stored_json_with_control);
    }
}

fn retention<T: std::fmt::Debug>(text: &str, read: Read<T>) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read(text, &ProductionControl::new(&memory, &token, &token)).unwrap());
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
    for limit in (0..peak).step_by(13).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        assert!(read(text, &ProductionControl::new(&memory, &token, &token)).is_err());
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_role_records_admit_native_allocations_and_retire_failed_prefixes() {
    retention(ROLE, RoleDefinition::from_stored_json_with_control);
    retention(&membership(), RoleMembership::from_stored_json_with_control);
}

fn cancellation<T: std::fmt::Debug>(text: &str, read: Read<T>) {
    for original_owner in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let selected = if original_owner { &original } else { &invoking };
        let control = ProductionControl::new(&memory, &original, &invoking);
        let output = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            selected,
            4096,
            || read(text, &control),
        );
        assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        assert!(memory.peak() >= 4096);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_role_records_cancel_both_owners_after_native_name_admission() {
    cancellation(
        &ROLE.replace("reader_역할", &"x".repeat(16384)),
        RoleDefinition::from_stored_json_with_control,
    );
    cancellation(
        &membership().replace("bound_역할", &"x".repeat(16384)),
        RoleMembership::from_stored_json_with_control,
    );
}

#[test]
fn stored_duplicate_role_attributes_use_constant_retained_workspace() {
    let mut peaks = Vec::new();
    for count in [1, 4096] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let text = ROLE.replace(
            "\"Login\",\"Inherit\"",
            &std::iter::repeat_n("\"Login\"", count)
                .collect::<Vec<_>>()
                .join(","),
        );
        let value = RoleDefinition::from_stored_json_with_control(
            &text,
            &ProductionControl::new(&memory, &token, &token),
        )
        .unwrap();
        assert_eq!(value.attributes.len(), 1);
        peaks.push(memory.peak());
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(peaks[0], peaks[1]);
}
