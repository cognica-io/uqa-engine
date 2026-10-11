//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` observations at both signed interval time-field limits.

use super::*;

#[test]
fn interval_time_input_matches_postgresql_at_signed_carrier_limits() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("interval_limits.json")).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(reference.as_array().unwrap().len(), 23);
    for case in reference.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let value = TemporalValue::interval_input_with_control(input, &control).unwrap();
        if let Some(expected) = case["expected"].as_str() {
            let value = value.unwrap();
            assert_eq!(value.to_sql_string(), expected, "{input}");
            assert_eq!(TemporalValue::parse_interval(input), Some(value));
        } else {
            let error = match case["error"]["sqlstate"].as_str().unwrap() {
                "22015" => TemporalInputError::IntervalFieldOverflow,
                "22007" => TemporalInputError::InvalidSyntax,
                other => panic!("unexpected PostgreSQL diagnostic {other}"),
            };
            assert_eq!(value, Err(error), "{input}");
            assert_eq!(TemporalValue::parse_interval(input), None);
        }
        assert_eq!(memory.used(), 0);
    }
}
