//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
