//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered-set mode results captured independently from `PostgreSQL` 18.

use super::*;

fn check_reference(prepared: bool) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-execution/src/aggregation/tests/pg18_mode.json"
    )))
    .unwrap();
    let engine = engine();
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let expected = case["winner"].as_i64().map_or(Value::Null, Value::Int);
        if prepared {
            engine
                .sql(&format!("PREPARE ordered_mode AS {sql}"), &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(scalar(&engine, "EXECUTE ordered_mode"), expected, "{sql}");
            engine.sql("DEALLOCATE ordered_mode", &[]).unwrap();
        } else {
            assert_eq!(scalar(&engine, sql), expected, "{sql}");
        }
    }
}

#[test]
fn ordered_mode_matches_postgresql_equivalence_and_ties() {
    check_reference(false);
}

#[test]
fn prepared_ordered_mode_matches_postgresql_equivalence_and_ties() {
    check_reference(true);
}
