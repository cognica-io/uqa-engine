//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    json::{JsonDocument, JsonScalarProtocol},
    memory::MemoryBudget,
    CancellationToken,
};

#[derive(Debug, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    One,
    Two,
}

#[test]
fn generic_unit_enums_preserve_cardinality_tag_and_payload_error_order() {
    let protocol = JsonScalarProtocol::from_values(
        &serde_json::Number::from(1_u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    for text in [
        r#""one""#,
        r#""two""#,
        r#""unknown""#,
        r#"{"one":null}"#,
        r#"{"one":true}"#,
        r#"{"unknown":false}"#,
        r#"{"unknown":false,"two":null}"#,
        "{}",
        "[]",
        "null",
        "1.5",
        "true",
    ] {
        let expected = serde_json::from_value::<Mode>(serde_json::from_str(text).unwrap());
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let value = JsonDocument::read_with_control(text.as_bytes(), &control, protocol).unwrap();
        let actual = crate::json::work::inline(value.root().read_unit_variant_with_work(
            &["one", "two"],
            &control,
            &mut (),
        ));
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(expected, if actual == 0 { Mode::One } else { Mode::Two });
            }
            (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
                assert_eq!(actual.to_string(), expected.to_string(), "{text}");
                assert_eq!(actual.is_data(), expected.is_data());
            }
            other => panic!("generic enum differs: {other:?}"),
        }
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn generic_field_context_preserves_admission_and_redacts_retained_payloads() {
    static WEIGHT: crate::json::JsonMessageFormat =
        crate::json::JsonMessageFormat::new("stored weight: ", "", "invalid stored weight");
    let secret = "private🙂".repeat(8192);
    let text = serde_json::to_string(&secret).unwrap();
    let memory = MemoryBudget::new(2 << 20);
    let diagnostics = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let protocol = JsonScalarProtocol::from_values(
        &serde_json::Number::from(1_u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let value = JsonDocument::read_with_control(text.as_bytes(), &control, protocol).unwrap();
    let error = crate::json::work::inline(value.root().read_f64_with_work(
        &ProductionControl::new(&diagnostics, &token, &token),
        &mut (),
    ))
    .unwrap_err();
    let JsonInputError::Diagnostic(error) = error else {
        panic!("typed field diagnostic");
    };
    let position = JsonPosition {
        byte_offset: text.len(),
        line: 1,
        column: text.len(),
    };
    let error = error
        .with_context(&WEIGHT, JsonPosition::unlocated(), &diagnostics)
        .unwrap()
        .at_position(position);
    assert!(error.to_string().contains(&secret));
    assert!(diagnostics.used() >= secret.len());
    drop(value);
    assert_eq!(memory.used(), 0);
    let error = error.into_type_only();
    assert_eq!(diagnostics.used(), size_of::<JsonDiagnostic>());
    assert_eq!(error.position(), position);
    assert_eq!(
        error.to_string(),
        format!(
            "stored weight: invalid type: string, expected f64 at line 1 column {}",
            text.len()
        )
    );
    assert!(!format!("{error:?}").contains("private"));
    drop(error);
    assert_eq!(diagnostics.used(), 0);
}
