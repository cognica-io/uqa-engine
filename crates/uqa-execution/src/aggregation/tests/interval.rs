//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` interval aggregates, including finite overflow beside infinity.

use super::super::{aggregate_value, AggregateAccumulator};
use uqa_core::{TemporalValue, Value};
use uqa_sql::SQLError;

#[test]
fn interval_json_keys_and_values_share_sql_output_in_memory_and_spill() {
    let _style = uqa_sql::expr::IntervalStyleScope::enter(uqa_core::TemporalIntervalStyle::Iso8601);
    let values = [
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: -1,
            micros: 3_600_000_000,
        }),
        Value::Temporal(TemporalValue::Interval {
            months: 14,
            days: 3,
            micros: 14_400_000_000,
        }),
    ];
    // Captured with PostgreSQL 18.4 json[b]_object_agg under iso_8601.
    for (name, expected) in [
        (
            "json_object_agg",
            Value::Json(r#"{ "P-1DT1H" : "P-1DT1H", "P1Y2M3DT4H" : "P1Y2M3DT4H" }"#.into()),
        ),
        (
            "jsonb_object_agg",
            Value::JsonB(r#"{"P-1DT1H": "P-1DT1H", "P1Y2M3DT4H": "P1Y2M3DT4H"}"#.into()),
        ),
    ] {
        for budget in [1, 4096] {
            let mut accumulator = AggregateAccumulator::builtin_with_budget(name, budget);
            for value in &values {
                accumulator
                    .observe(&Value::List(vec![value.clone(), value.clone()]))
                    .unwrap();
            }
            assert_eq!(aggregate_value(name, &accumulator, None).unwrap(), expected);
        }
    }
}

fn inputs(case: &serde_json::Value) -> Vec<Value> {
    case["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            value.as_str().map_or(Value::Null, |text| {
                Value::Temporal(
                    TemporalValue::parse_interval(text)
                        .unwrap_or_else(|| panic!("interval input {text}")),
                )
            })
        })
        .collect()
}

fn matches_reference(observed: &Result<Value, SQLError>, case: &serde_json::Value) -> bool {
    let expected_error = &case["error"];
    match observed {
        Err(error) => {
            !expected_error.is_null()
                && error.sqlstate() == expected_error["sqlstate"].as_str()
                && error.to_string() == expected_error["message"].as_str().unwrap()
                && error.detail() == expected_error["detail"].as_str()
                && error.hint() == expected_error["hint"].as_str()
        }
        Ok(value) if expected_error.is_null() => {
            let expected = &case["results"][0]["rows"][0][0];
            match value {
                Value::Null => expected.is_null(),
                Value::Temporal(value) => Some(value.to_sql_string().as_str()) == expected.as_str(),
                _ => false,
            }
        }
        Ok(_) => false,
    }
}

#[test]
fn interval_aggregate_values_and_overflow_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("interval.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 110);
    let mut failures = Vec::new();
    for case in cases.iter().filter(|case| case.get("inputs").is_some()) {
        let name = case["name"].as_str().unwrap();
        let mut accumulator = AggregateAccumulator::builtin(name);
        let observed = inputs(case)
            .iter()
            .try_for_each(|value| accumulator.observe(value))
            .and_then(|()| aggregate_value(name, &accumulator, None));
        if !matches_reference(&observed, case) {
            failures.push(format!(
                "{}\nexpected: {case}\nobserved: {observed:?}",
                case["id"]
            ));
        }
        assert!(accumulator.values.rows.is_empty());
        assert!(accumulator.values.runs.is_empty());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn interval_aggregate_partial_states_preserve_infinities_and_finite_overflow_after_spill() {
    use super::super::partial_state::{
        decode_partial_group, encode_partial_group, merge_accumulators,
    };
    use crate::{Batch, RowSchema, ScalarExpr, SpillBuffer};

    let reference: serde_json::Value = serde_json::from_str(include_str!("interval.json")).unwrap();
    let cases: Vec<_> = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case.get("inputs").is_some())
        .collect();
    let mut rows = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        // Each input gets its own partial state; the leading empty state also
        // exercises the identity for empty and all-NULL aggregates.
        for value in std::iter::once(Value::Null).chain(inputs(case)) {
            let mut state = AggregateAccumulator::builtin(name);
            state.observe(&value).unwrap();
            rows.push(encode_partial_group(
                vec![Value::Int(index as i64)],
                vec![state],
            ));
        }
    }
    let schema = RowSchema::new(vec!["reference".into(), "state".into()]);
    let mut spill = SpillBuffer::new(0);
    spill.push(Batch::from_physical_rows(schema, rows)).unwrap();
    let path = spill.spill_path().unwrap().to_owned();
    assert!(path.exists());
    let mut restored = Vec::new();
    for case in &cases {
        restored.push(Ok(AggregateAccumulator::builtin(
            case["name"].as_str().unwrap(),
        )));
    }
    for batch in spill.drain_all().unwrap() {
        for row in batch.rows {
            let Some(Value::Int(index)) = row.value(0) else {
                panic!("reference index")
            };
            let index = usize::try_from(*index).unwrap();
            let target = ScalarExpr::Func {
                name: cases[index]["name"].as_str().unwrap().into(),
                binding: None,
                args: vec![],
                distinct: false,
                order_by: vec![],
                order_syntax: uqa_sql::ast::FunctionCallSyntax::default(),
                filter: None,
            };
            let (_, mut states) = decode_partial_group(row, &[target], 1024, 1).unwrap();
            if let Ok(accumulator) = &mut restored[index] {
                if let Err(error) = merge_accumulators(accumulator, states.pop().unwrap(), None) {
                    restored[index] = Err(error);
                }
            }
        }
    }
    assert!(!path.exists());
    for (case, state) in cases.into_iter().zip(restored) {
        let observed =
            state.and_then(|state| aggregate_value(case["name"].as_str().unwrap(), &state, None));
        assert!(
            matches_reference(&observed, case),
            "{case}\nobserved: {observed:?}"
        );
    }
}
