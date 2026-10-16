//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn scalar_range_window_peers_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-execution/src/window/tests/range_peers/postgresql.json"
    ))
    .unwrap();
    let engine = Engine::new();
    let mut mismatches = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        match engine.sql(case["sql"].as_str().unwrap(), &[]) {
            Ok(result) => {
                assert_eq!(result.rows.len(), 1, "{}", case["id"]);
                let actual = &result.rows[0][&result.columns[0]];
                let expected = Value::Int(case["count"].as_i64().unwrap());
                if actual != &expected {
                    mismatches.push(format!("{}: {actual:?}, expected {expected:?}", case["id"]));
                }
            }
            Err(error) => mismatches.push(format!("{}: {error}", case["id"])),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
