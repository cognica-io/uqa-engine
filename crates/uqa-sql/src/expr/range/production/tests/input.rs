//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input shape, subtype conversion and their ordered diagnostics from `PostgreSQL`.

use super::*;

#[test]
fn range_input_and_diagnostics_match_independent_postgresql_reference() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../../tests/parity/pg18/range_input_oracle.expected.json"
    ))
    .unwrap();
    let memory = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut failures = Vec::new();
    let mut observations = 0;
    for case in reference["cases"].as_array().unwrap() {
        let Some(kind) = case["kind"].as_str() else {
            continue;
        };
        let subtype = match kind {
            "int4range" | "int4multirange" => RangeSubtype::Integer,
            "int8range" | "int8multirange" => RangeSubtype::BigInteger,
            "numrange" | "nummultirange" => RangeSubtype::Numeric,
            "daterange" | "datemultirange" => RangeSubtype::Date,
            "tsrange" | "tsmultirange" => RangeSubtype::Timestamp,
            "tstzrange" | "tstzmultirange" => RangeSubtype::TimestampTz,
            _ => panic!("unknown independent reference kind {kind}"),
        };
        let text = case["input"].as_str().unwrap();
        let result = if kind.contains("multirange") {
            canonical_multirange_text_with_control(text, subtype, &control)
        } else {
            canonical_range_text_with_control(text, subtype, &control)
        };
        let actual = match result {
            Ok(value) => serde_json::json!({"value": &**value, "error": null}),
            Err(error) => serde_json::json!({"value": null, "error": {
                "sqlstate": error.sqlstate(), "message": error.to_string(),
                "detail": error.detail(), "hint": error.hint(),
            }}),
        };
        let expected = serde_json::json!({
            "value": case["results"][0]["rows"][0][0], "error": case["error"],
        });
        if actual != expected {
            failures.push(format!(
                "{}: actual {actual}; expected {expected}",
                case["id"]
            ));
        }
        assert_eq!(memory.used(), 0, "{}", case["id"]);
        observations += 1;
    }
    assert_eq!(observations, 117);
    assert!(
        failures.is_empty(),
        "{} differences: {}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn range_structure_errors_precede_bound_admission() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../../tests/parity/pg18/range_input_oracle.expected.json"
    ))
    .unwrap();
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for text in ["[bad,2x", "[bad,2,3)", "[bad,2)junk", "[\"unterminated,20)"] {
        let expected = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["kind"] == "int4range" && case["input"] == text)
            .unwrap();
        let error =
            canonical_range_text_with_control(text, RangeSubtype::Integer, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            expected["error"]["sqlstate"].as_str(),
            "{text}"
        );
        assert_eq!(
            error.to_string(),
            expected["error"]["message"].as_str().unwrap(),
            "{text}"
        );
        assert_eq!(
            error.detail(),
            expected["error"]["detail"].as_str(),
            "{text}"
        );
        assert_eq!(memory.peak(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn empty_multirange_members_do_not_retain_workspace() {
    for member in ["empty", "empty,[1,1)"] {
        let mut peaks = Vec::new();
        for count in [1, 512] {
            let input = format!("{{{}}}", vec![member; count].join(","));
            let memory = MemoryBudget::new(4096);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let output =
                canonical_multirange_text_with_control(&input, RangeSubtype::Integer, &control)
                    .unwrap();
            assert_eq!(&**output, "{}");
            assert_eq!(memory.used(), output.reserved_bytes());
            drop(output);
            assert_eq!(memory.used(), 0);
            peaks.push(memory.peak());
        }
        assert_eq!(peaks[0], peaks[1], "{member}");
    }
}
