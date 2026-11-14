//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::work::inline;
use uqa_core::{
    json::{JsonDocument, JsonScalarProtocol},
    memory::MemoryBudget,
    CancellationToken,
};

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol {
        number: "unused-number-protocol",
        raw: "unused-raw-protocol",
    }
}

fn equal(actual: Option<&[LegacyRoutineAclEntry]>, expected: Option<&[LegacyRoutineAclEntry]>) {
    match (actual, expected) {
        (None, None) => (),
        (Some(actual), Some(expected)) => {
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.role, expected.role);
                assert_eq!(actual.grantor, expected.grantor);
                assert_eq!(actual.grant_option, expected.grant_option);
            }
        }
        _ => panic!("absent and explicit ACL differ"),
    }
}

fn compare(text: Option<&str>) {
    let value: serde_json::Value = serde_json::from_str(text.unwrap_or("null")).unwrap();
    let expected = serde_json::from_value::<Option<Vec<LegacyRoutineAclEntry>>>(value);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let document = text.map(|text| {
        JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap()
    });
    let actual = inline(read_legacy_acl_with_work(
        document.as_ref().map(JsonDocument::root),
        &control,
        &mut (),
    ));
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => equal(actual.as_deref(), expected.as_deref()),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text:?}");
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.is_syntax(), expected.is_syntax());
        }
        (expected, actual) => panic!(
            "{text:?}: expected success {}; actual success {}",
            expected.is_ok(),
            actual.is_ok()
        ),
    }
    drop(document);
    assert_eq!(memory.used(), 0);
}

#[test]
fn projected_legacy_routine_acl_preserves_complete_typed_fields_and_error_order() {
    compare(None);
    for text in ["null", "[]", "{}", "false", "1", r#""text""#] {
        compare(Some(text));
    }
    for role in [
        r#""PUBLIC""#,
        r#""owner""#,
        r#"{"kind":"role","name":"PUBLIC"}"#,
        r#"{"kind":"public"}"#,
        r#"["role","owner"]"#,
        "null",
        "false",
        "1",
        "[]",
        "{}",
    ] {
        for grantor in ["null", r#""owner""#, r#""한🙂""#, "false", "1", "[]", "{}"] {
            for option in ["true", "false", "null", "1", r#""text""#, "[]", "{}"] {
                compare(Some(&format!(
                    r#"[{{"role":{role},"grantor":{grantor},"grant_option":{option}}}]"#
                )));
                compare(Some(&format!("[[{role},{grantor},{option}]]")));
                compare(Some(&format!("[[{role},{grantor},{option},null]]")));
            }
        }
        compare(Some(&format!(
            r#"[{{"role":{role},"grant_option":false}}]"#
        )));
        compare(Some(&format!("[[{role}]]")));
        compare(Some(&format!("[[{role},null]]")));
    }
    for entry in [
        "null",
        "true",
        "1",
        r#""text""#,
        "{}",
        "[]",
        r#"{"grantor":null,"grant_option":false}"#,
        r#"{"role":"PUBLIC"}"#,
        r#"{"role":"PUBLIC","grant_option":false,"ignored":{"kind":null}}"#,
    ] {
        compare(Some(&format!("[{entry}]")));
        compare(Some(&format!(
            r#"[{{"role":"PUBLIC","grant_option":false}},{entry}]"#
        )));
    }
}

#[test]
fn projected_legacy_routine_acl_retains_exact_owned_payloads_without_source_aliases() {
    let text = format!(
        r#"[{{"role":{{"kind":"role","name":"{}"}},"grantor":"{}","grant_option":false}},{{"role":"PUBLIC","grant_option":false}}]"#,
        "한🙂".repeat(8192),
        "grantor🙂".repeat(8192)
    );
    let source_memory = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let document = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&source_memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            inline(read_legacy_acl_with_work(
                Some(document.root()),
                &control,
                &mut (),
            ))
            .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(measured.bytes_max).unwrap() <= memory.peak());
    drop(document);
    assert_eq!(source_memory.used(), 0);
    let expected = serde_json::from_str::<Option<Vec<LegacyRoutineAclEntry>>>(&text).unwrap();
    equal(output.as_ref().unwrap().as_deref(), expected.as_deref());
    drop(output);
    assert_eq!(memory.used(), 0);
}

mod resources;
