//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `interval_out` observations, including mixed signs and carrier extrema.

use super::*;
use crate::TemporalInputError;
use crate::{memory::MemoryBudget, CancellationToken};

fn style(name: &str) -> TemporalIntervalStyle {
    match name {
        "postgres" => TemporalIntervalStyle::Postgres,
        "postgres_verbose" => TemporalIntervalStyle::PostgresVerbose,
        "sql_standard" => TemporalIntervalStyle::SqlStandard,
        "iso_8601" => TemporalIntervalStyle::Iso8601,
        other => panic!("unknown reference style {other}"),
    }
}

#[test]
fn interval_output_matches_postgresql_for_each_style() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/oracle.json")).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 588);
    for case in cases {
        let Some(expected) = case["expected"].as_str() else {
            continue; // A rejected input has no output carrier.
        };
        let value = TemporalValue::Interval {
            months: i32::try_from(case["fields"][0].as_i64().unwrap()).unwrap(),
            days: i32::try_from(case["fields"][1].as_i64().unwrap()).unwrap(),
            micros: case["fields"][2].as_i64().unwrap(),
        };
        let style = style(case["style"].as_str().unwrap());
        let text = value
            .to_sql_string_in_interval_style_with_control(style, &control)
            .unwrap();
        assert_eq!(&**text, expected, "{case}");
        assert_eq!(memory.used(), text.capacity());
        if style == TemporalIntervalStyle::Postgres {
            assert_eq!(value.to_sql_string(), expected, "canonical output: {case}");
        }
        drop(text);
        assert_eq!(memory.used(), 0, "{case}");
    }
}

#[test]
fn interval_style_production_obeys_memory_and_both_cancellation_scopes() {
    let value = TemporalValue::Interval {
        months: -14,
        days: 3,
        micros: -14_706_120_000,
    };
    for style in [
        TemporalIntervalStyle::Postgres,
        TemporalIntervalStyle::PostgresVerbose,
        TemporalIntervalStyle::SqlStandard,
        TemporalIntervalStyle::Iso8601,
    ] {
        let empty = MemoryBudget::new(0);
        let token = CancellationToken::new();
        assert!(matches!(
            value.to_sql_string_in_interval_style_with_control(
                style,
                &ProductionControl::new(&empty, &token, &token)
            ),
            Err(ValueRetentionError::Memory(_))
        ));
        for input in ["-1 02:03:04", "P1Y2M3DT4H"] {
            assert!(matches!(
                TemporalValue::interval_input_in_style_with_control(
                    input,
                    style,
                    &ProductionControl::new(&empty, &token, &token)
                ),
                Err(ValueRetentionError::Memory(_))
            ));
        }
        assert_eq!(empty.used(), 0);
        for original_cancelled in [true, false] {
            let memory = MemoryBudget::new(4096);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                value.to_sql_string_in_interval_style_with_control(
                    style,
                    &ProductionControl::new(&memory, &original, &invoking)
                ),
                Err(ValueRetentionError::Cancelled(_))
            ));
            for input in ["-1 02:03:04", "P1Y2M3DT4H"] {
                assert!(matches!(
                    TemporalValue::interval_input_in_style_with_control(
                        input,
                        style,
                        &ProductionControl::new(&memory, &original, &invoking)
                    ),
                    Err(ValueRetentionError::Cancelled(_))
                ));
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn interval_input_matches_postgresql_fields_and_errors_in_each_style() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/oracle.json")).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut differences = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let style = style(case["style"].as_str().unwrap());
        let actual = TemporalValue::interval_input_in_style_with_control(
            case["input"].as_str().unwrap(),
            style,
            &control,
        )
        .unwrap();
        if let Some(state) = case["error"]["sqlstate"].as_str() {
            let error = match state {
                "22007" => TemporalInputError::InvalidSyntax,
                "22015" => TemporalInputError::IntervalFieldOverflow,
                "22008" => TemporalInputError::IntervalOutOfRange,
                other => panic!("unexpected reference SQLSTATE {other}"),
            };
            if actual != Err(error) {
                differences.push(format!("{case}: {actual:?}"));
            }
        } else {
            match actual {
                Ok(TemporalValue::Interval {
                    months,
                    days,
                    micros,
                }) => {
                    // Interval equality flattens months/days: compare the physical fields themselves.
                    if serde_json::json!([months, days, micros]) != case["fields"] {
                        differences.push(format!("{case}: [{months}, {days}, {micros}]"));
                    }
                }
                other => differences.push(format!("{case}: {other:?}")),
            }
        }
        assert_eq!(memory.used(), 0, "{case}");
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
