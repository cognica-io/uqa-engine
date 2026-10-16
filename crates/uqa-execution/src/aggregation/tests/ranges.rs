//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scalar_range_aggregate_distinct_preserves_classes_across_merge_runs() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../uqa-sql/src/expr/range/comparison/postgresql.json"
    ))
    .unwrap();
    for family in reference["families"].as_array().unwrap() {
        let name = family["kind"].as_str().unwrap();
        let ty = uqa_sql::ColumnType::from_sql_name(name).unwrap();
        let values = family["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                uqa_sql::expr::cast_value(&Value::Str(v.as_str().unwrap().into()), name).unwrap()
            })
            .collect::<Vec<_>>();
        let representatives = (0..values.len())
            .filter(|&index| !(0..index).any(|prior| family["comparisons"][index][prior] == 0))
            .collect::<Vec<_>>();
        for budget in [1, 256, 64 * 1024] {
            let template = AggregateAccumulatorTemplate::builtin("count", Some(&ty))
                .with_ordering_types(vec![Some(ty.clone())]);
            let mut accumulator = template.instantiate(budget);
            for _ in 0..(AGGREGATE_MERGE_FAN_IN + 1).div_ceil(values.len()) {
                for value in values.iter().rev() {
                    accumulator
                        .distinct
                        .insert(value, Vec::new(), None)
                        .unwrap();
                }
            }
            if budget == 1 {
                assert!(!accumulator.distinct.values.runs.is_empty());
            }
            let mut observed = Vec::new();
            accumulator
                .distinct
                .for_each(None, |value| {
                    observed.push(value.clone());
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                observed.len(),
                representatives.len(),
                "{name}, budget={budget}"
            );
            for representative in &representatives {
                let matches = observed
                    .iter()
                    .filter(|value| {
                        let index = values
                            .iter()
                            .position(|input| input.has_same_representation(value))
                            .unwrap();
                        family["comparisons"][index][*representative] == 0
                    })
                    .count();
                assert_eq!(matches, 1, "{name}: {representative}, budget={budget}");
            }
        }
    }
}
