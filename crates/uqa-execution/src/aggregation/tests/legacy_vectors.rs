//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn dimensionless_oidvector() -> Value {
    Value::LegacyVector(
        uqa_core::LegacyVectorValue::try_from_array(
            uqa_core::LegacyVectorKind::Oid,
            ArrayValue::with_lower_bounds(vec![], vec![]).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn legacy_vector_nested_extrema_propagate_comparison_errors_in_partial_merges() {
    let scalar = dimensionless_oidvector();
    let nested = Value::Array(ArrayValue::try_new(vec![scalar.clone()]).unwrap());
    for name in ["min", "max"] {
        let mut valid = AggregateAccumulator::builtin(name);
        valid.observe(&scalar).unwrap();
        valid.observe(&scalar).unwrap();
        let mut left = AggregateAccumulator::builtin(name);
        left.observe(&nested).unwrap();
        let mut right = AggregateAccumulator::builtin(name);
        right.observe(&nested).unwrap();
        let error = super::super::partial_state::merge_accumulators(&mut left, right).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        let error = left.observe(&nested).unwrap_err();
        assert_eq!(error.to_string(), "array is not a valid oidvector");
    }
}

#[derive(Default)]
struct ObservedValues(Vec<Value>);

impl SQLAggregateState for ObservedValues {
    fn observe(&mut self, values: &[Value]) -> Result<(), SQLError> {
        self.0.extend_from_slice(values);
        Ok(())
    }
    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::List(self.0.clone()))
    }
}

#[test]
fn legacy_vector_ordered_aggregates_preserve_errors_across_memory_and_merge_runs() {
    for budget in [1, 1 << 20] {
        for count in [1, 2, 18] {
            let mut builtin = AggregateValueBuffer::new(budget);
            let mut registered = RegisteredAggregateBuffer::new(budget);
            let keys = vec![(dimensionless_oidvector(), false)];
            let builtin_result = (|| {
                for value in 0..count {
                    builtin.push(Value::Int(value), keys.clone())?;
                }
                builtin.ordered_values()
            })();
            let mut state = ObservedValues::default();
            let registered_result = (|| {
                for value in 0..count {
                    registered.push(vec![Value::Int(value)], keys.clone())?;
                }
                registered.observe_ordered_into(&mut state)
            })();
            if count == 1 {
                assert_eq!(builtin_result.unwrap(), vec![Value::Int(0)]);
                registered_result.unwrap();
                assert_eq!(state.0, vec![Value::Int(0)]);
            } else {
                for error in [builtin_result.unwrap_err(), registered_result.unwrap_err()] {
                    assert_eq!(error.sqlstate(), Some("42804"));
                    assert_eq!(error.to_string(), "array is not a valid oidvector");
                }
            }
        }
        let mut buffer = AggregateValueBuffer::new(budget);
        for value in [1, 0] {
            buffer
                .push(
                    Value::Int(value),
                    vec![
                        (Value::Int(value), false),
                        (dimensionless_oidvector(), false),
                    ],
                )
                .unwrap();
        }
        assert_eq!(
            buffer.ordered_values().unwrap(),
            vec![Value::Int(0), Value::Int(1)]
        );
    }
}

#[test]
fn legacy_vector_extrema_use_postgresql_array_aggregate_order() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../uqa-core/src/types/tests/pg18_legacy_vectors.json"
    ))
    .unwrap();
    for group in oracle["types"].as_array().unwrap() {
        let ty = group["type"].as_str().unwrap();
        let case = group["queries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["sql"].as_str().unwrap().starts_with("SELECT min(v)"))
            .unwrap();
        let values: Vec<_> = ["2", "1 9"]
            .into_iter()
            .map(|text| cast_value(&Value::Str(text.into()), ty).unwrap())
            .collect();
        for reversed in [false, true] {
            for (index, name) in ["min", "max"].into_iter().enumerate() {
                let mut accumulator = AggregateAccumulator::builtin(name);
                for position in if reversed { [1, 0] } else { [0, 1] } {
                    accumulator.observe(&values[position]).unwrap();
                }
                let value = aggregate_value(name, &accumulator).unwrap();
                assert_eq!(
                    cast_value(&value, "text").unwrap(),
                    Value::Str(case["rows"][0][index].as_str().unwrap().into()),
                    "{ty} {name} reversed={reversed}"
                );
            }
        }
    }
}

#[test]
fn legacy_vector_json_object_keys_match_postgresql_errors_in_memory_and_spill() {
    for ty in ["int2vector", "oidvector"] {
        let value = cast_value(&Value::Str("1 2".into()), ty).unwrap();
        for name in ["json_object_agg", "jsonb_object_agg"] {
            for budget in [1, 1024] {
                let mut accumulator = AggregateAccumulator::builtin_with_budget(name, budget);
                accumulator
                    .observe(&Value::List(vec![value.clone(), Value::Int(1)]))
                    .unwrap();
                let error = aggregate_value(name, &accumulator).unwrap_err();
                assert_eq!(error.sqlstate(), Some("22023"));
                assert_eq!(
                    error.to_string(),
                    "key value must be scalar, not array, composite, or json"
                );
            }
        }
    }
}

#[test]
fn legacy_vector_distinct_aggregates_compare_before_deduplicating_in_memory_and_spill() {
    for budget in [1, 1 << 20] {
        for count in [1, 2, 18] {
            for registered in [false, true] {
                let mut acc = if registered {
                    AggregateAccumulator::registered_with_budget(
                        Arc::new(ObservedValues::default),
                        budget,
                    )
                } else {
                    AggregateAccumulator::builtin_with_budget("count", budget)
                };
                let mut input = dimensionless_oidvector();
                if registered {
                    input = Value::List(vec![input]);
                }
                let result = (|| {
                    for _ in 0..count {
                        acc.distinct.insert(&input, Vec::new())?;
                    }
                    aggregate_value("count", &acc)
                })();
                if count == 1 {
                    let expected = if registered { input } else { Value::Int(1) };
                    assert_eq!(result.unwrap(), expected);
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(error.sqlstate(), Some("42804"));
                    assert_eq!(error.to_string(), "array is not a valid oidvector");
                }
            }
        }
    }
}
