//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde::{de::DeserializeOwned, Serialize};
use uqa_core::{memory::MemoryBudget, CancellationToken};

type Read<T> = fn(&str, &ProductionControl<'_>) -> Result<Produced<T>, SQLError>;

fn owner() -> serde_json::Value {
    serde_json::to_value(RoleIdentity::BOOTSTRAP).unwrap()
}

fn enumeration() -> String {
    serde_json::json!({
        "object_id": vec![1u8; 16], "oid": 20000, "array_oid": 20001, "array_name": "_mood",
        "identity": {"schema":"public", "name":"mood"}, "owner":owner(),
        "labels": [{"oid":20002,"key":"80","label":"정확한 값","sort_order":-0.0}],
        "usage_acl": [{"role":null,"grantor":owner(),"grant_option":false}],
    })
    .to_string()
}

fn shell() -> String {
    serde_json::json!({"object_id":vec![2u8;16],"oid":20003,"identity":{"schema":"public","name":"pending"},"owner":owner()}).to_string()
}

fn equivalent<T: DeserializeOwned + Serialize + std::fmt::Debug>(text: &str, read: Read<T>) {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let expected = serde_json::from_str::<T>(text);
    let actual = read(text, &ProductionControl::new(&memory, &token, &token));
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?}, {expected:?}"
    );
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            serde_json::to_value(&*actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{text}"
        );
    }
    assert_eq!(memory.used(), 0);
}

fn mutations<T: DeserializeOwned + Serialize + std::fmt::Debug>(text: &str, read: Read<T>) {
    let original: serde_json::Value = serde_json::from_str(text).unwrap();
    for field in original.as_object().unwrap().keys() {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        equivalent(&missing.to_string(), read);
        equivalent(
            &format!(r#"{{"{field}":null,{}}}"#, &text[1..text.len() - 1]),
            read,
        );
        let mut changed = original.clone();
        changed[field] = serde_json::Value::Null;
        equivalent(&changed.to_string(), read);
    }
    for (end, _) in text.char_indices() {
        equivalent(&text[..end], read);
    }
    equivalent(&format!("{text} []"), read);
    equivalent(
        &format!(r#"{{"unknown":null,{}}}"#, &text[1..text.len() - 1]),
        read,
    );
}

#[test]
fn stored_type_input_preserves_strict_fields_optional_acls_and_positional_forms() {
    let read_enum = StoredEnum::from_stored_json_with_control;
    let read_shell = StoredShellType::from_stored_json_with_control;
    equivalent(&enumeration(), read_enum);
    equivalent(&shell(), read_shell);
    mutations(&enumeration(), read_enum);
    mutations(&shell(), read_shell);
    let value: serde_json::Value = serde_json::from_str(&enumeration()).unwrap();
    let fields = [
        "object_id",
        "oid",
        "array_oid",
        "array_name",
        "identity",
        "owner",
        "labels",
        "usage_acl",
    ];
    let mut positional =
        serde_json::Value::Array(fields.iter().map(|field| value[*field].clone()).collect());
    equivalent(&positional.to_string(), read_enum);
    positional.as_array_mut().unwrap().pop();
    equivalent(&positional.to_string(), read_enum);
    let mut nested = value.clone();
    nested["identity"] = serde_json::json!(["public", "mood"]);
    nested["owner"] = serde_json::json!([10, RoleIdentity::BOOTSTRAP.object_id]);
    nested["labels"] = serde_json::json!([[20002, "80", "position", 0.25]]);
    nested["usage_acl"] = serde_json::json!([[null, owner(), false]]);
    equivalent(&nested.to_string(), read_enum);
    for path in ["identity", "owner"] {
        let mut nested = value.clone();
        nested[path]["unknown"] = serde_json::json!([{"nested":"ignored"}]);
        equivalent(&nested.to_string(), read_enum);
    }
    for acl in ["null", "[]", "[{}]", "[null]", "[[]]"] {
        let mut nested = value.clone();
        nested["usage_acl"] = serde_json::from_str(acl).unwrap();
        equivalent(&nested.to_string(), read_enum);
    }
}

#[test]
fn stored_enum_input_preserves_float4_bits_without_generic_number_allocation() {
    let fixture = enumeration();
    assert!(fixture.contains("-0.0"));
    let mut numbers = [
        "-0",
        "0",
        "-0.0",
        "0.0",
        "1",
        "1.000000059604644775390625",
        "1.000000059604644775390626",
        "3.4028235e38",
        "3.4028236e38",
        "1e-45",
        "1e-400",
        "1e400",
        "18446744073709551615",
        "18446744073709551616",
    ]
    .map(str::to_owned)
    .to_vec();
    numbers.extend([
        format!("0.{}1", "0".repeat(32768)),
        format!("1e{}0", "0".repeat(32768)),
        format!("0e{}", "9".repeat(32768)),
    ]);
    for bits in [
        1u32,
        0x007f_ffff,
        0x0080_0000,
        0x3eff_ffff,
        0x3f00_0001,
        0x7f7f_ffff,
        0x8000_0000,
        0x8080_0000,
        0xff7f_ffff,
    ] {
        numbers.push(f32::from_bits(bits).to_string());
    }
    for number in numbers {
        let text = fixture.replace("-0.0", &number);
        let expected = serde_json::from_str::<StoredEnum>(&text);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let actual = StoredEnum::from_stored_json_with_control(
            &text,
            &ProductionControl::new(&memory, &token, &token),
        );
        assert_eq!(actual.is_ok(), expected.is_ok(), "{number}");
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(
                actual.labels[0].sort_order.to_bits(),
                expected.labels[0].sort_order.to_bits(),
                "{number}"
            );
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_enum_input_retains_core_key_validation_and_ignored_nested_fields() {
    for key in [
        "".into(),
        "0".into(),
        "00".into(),
        "g0".into(),
        "aB".into(),
        "0001".into(),
        "ff".repeat(MAX_ENUM_LABEL_KEY_BYTES),
        "ff".repeat(MAX_ENUM_LABEL_KEY_BYTES + 1),
    ] {
        equivalent(
            &enumeration().replace("\"80\"", &format!("\"{key}\"")),
            StoredEnum::from_stored_json_with_control,
        );
    }
    let text = enumeration();
    for marker in [
        r#""role":null,"#,
        r#""grantor":null,"#,
        r#""grant_option":null,"#,
    ] {
        equivalent(
            &text.replace(r#""usage_acl":[{"#, &format!(r#""usage_acl":[{{{marker}"#)),
            StoredEnum::from_stored_json_with_control,
        );
    }
    equivalent(
        &text.replace(r#""usage_acl":[{"#, r#""usage_acl":[{"ignored":"\uD800","#),
        StoredEnum::from_stored_json_with_control,
    );
}

fn retention<T: std::fmt::Debug>(text: &str, read: Read<T>) {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read(text, &ProductionControl::new(&memory, &token, &token)).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(
        usize::try_from(allocations.bytes_max).unwrap() <= memory.peak(),
        "{allocations:?}, {}",
        memory.peak()
    );
    let peak = memory.peak();
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(17).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        assert!(read(text, &ProductionControl::new(&memory, &token, &token)).is_err());
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_type_input_admits_native_key_name_label_and_acl_allocations() {
    retention(&enumeration(), StoredEnum::from_stored_json_with_control);
    retention(&shell(), StoredShellType::from_stored_json_with_control);
    retention(
        &enumeration().replace("-0.0", &format!("1e{}0", "0".repeat(32768))),
        StoredEnum::from_stored_json_with_control,
    );
}

fn cancellation<T: std::fmt::Debug>(text: &str, read: Read<T>) {
    for original_owner in [true, false] {
        let memory = MemoryBudget::new(4 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let selected = if original_owner { &original } else { &invoking };
        let control = ProductionControl::new(&memory, &original, &invoking);
        let result = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            selected,
            4096,
            || read(text, &control),
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_type_input_cancels_both_original_owners_after_native_admission() {
    cancellation(
        &enumeration().replace("_mood", &"유형".repeat(4096)),
        StoredEnum::from_stored_json_with_control,
    );
    cancellation(
        &shell().replace("pending", &"유형".repeat(4096)),
        StoredShellType::from_stored_json_with_control,
    );
}

mod adoption;
mod structured;
