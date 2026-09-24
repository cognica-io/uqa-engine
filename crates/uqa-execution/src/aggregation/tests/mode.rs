//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn check_reference(budget_bytes: usize) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!("pg18_mode.json")).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let kind = case["type"].as_str().unwrap();
        let values: Vec<_> = case["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| match value.as_str() {
                // Preserve alternate JSONB spellings at the native owner boundary.
                Some(text) if kind == "jsonb" => Value::JsonB(text.into()),
                Some(text) => cast_value(&Value::Str(text.into()), kind).unwrap(),
                None => Value::Null,
            })
            .collect();
        let expected = case["winner"]
            .as_u64()
            .map_or(Value::Null, |index| values[index as usize].clone());
        for reverse_inputs in [false, true] {
            let mut inputs = values.clone();
            if reverse_inputs {
                inputs.reverse();
            }
            let mut accumulator = AggregateAccumulator::builtin_with_budget("mode", budget_bytes);
            for value in inputs {
                let keys = vec![(value.clone(), case["descending"].as_bool().unwrap())];
                accumulator.observe_with_sort_keys(&value, keys).unwrap();
            }
            let should_spill =
                budget_bytes == 1 && values.iter().any(|v| !matches!(v, Value::Null));
            assert_eq!(!accumulator.values.runs.is_empty(), should_spill);
            assert_eq!(
                aggregate_value("mode", &accumulator).unwrap(),
                expected,
                "case={case}, budget={budget_bytes}, reversed={reverse_inputs}"
            );
        }
    }
}

#[test]
fn mode_memory_matches_postgresql_equivalence_and_ties() {
    check_reference(1024 * 1024);
}

#[test]
fn mode_spilled_matches_postgresql_equivalence_and_ties() {
    check_reference(1);
}

#[test]
fn mode_combines_distinct_nan_payloads_in_memory_and_spill() {
    for budget in [1, 1024 * 1024] {
        let mut accumulator = AggregateAccumulator::builtin_with_budget("mode", budget);
        for value in [
            Value::Float(f64::NAN),
            Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
            Value::Float(-f64::NAN),
            Value::Float(1.0),
            Value::Float(1.0),
        ] {
            accumulator
                .observe_with_sort_keys(&value, vec![(value.clone(), false)])
                .unwrap();
        }
        assert_eq!(
            aggregate_value("mode", &accumulator).unwrap(),
            Value::Float(f64::NAN)
        );
    }
}
