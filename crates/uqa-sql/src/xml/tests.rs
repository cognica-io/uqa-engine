//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn input_and_output_match_postgresql_18() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/xml_input.expected.json"
    ))
    .unwrap();
    let control = ProductionControl::uncontrolled();
    let mut differences = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let mode = if case["mode"] == "document" {
            XmlOption::Document
        } else {
            XmlOption::Content
        };
        let input = case["input"].as_str().unwrap();
        let mut notices = Vec::new();
        let validation = validate_with_notices(input, mode, |notice| {
            assert_eq!(notice.level, crate::NoticeLevel::Warning);
            notices
                .push(serde_json::json!({"sqlstate": notice.sqlstate, "message": notice.message}));
        });
        assert_eq!(serde_json::json!(notices), case["notices"], "{case}");
        match validation {
            Ok(()) => {
                if case.get("sqlstate").is_some() {
                    differences.push(format!("unexpected acceptance: {case}"));
                    continue;
                }
                let output = output_with_control(input, &control).unwrap();
                assert_eq!(output.as_str(), case["output"].as_str().unwrap(), "{case}");
                assert_eq!(input, case["stored"].as_str().unwrap(), "{case}");
            }
            Err(error) => {
                if error.sqlstate() != case["sqlstate"].as_str() {
                    differences.push(format!("unexpected rejection: {case}: {error}"));
                    continue;
                }
                assert_eq!(
                    error.to_string(),
                    case["message"].as_str().unwrap(),
                    "{case}"
                );
                assert_eq!(error.detail(), case["detail"].as_str(), "{case}");
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
