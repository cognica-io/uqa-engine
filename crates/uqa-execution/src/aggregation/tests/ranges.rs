//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scalar_range_mode_preserves_classes_in_memory_and_spill() {
    for kind in ["numrange", "nummultirange"] {
        let values = ["[1.0,3.00)", "[1.00,3.0)", "[4,5)"].map(|value| {
            let text = if kind == "nummultirange" {
                format!("{{{value}}}")
            } else {
                value.into()
            };
            cast_value(&Value::Str(text), kind).unwrap()
        });
        let range = uqa_sql::ColumnType::from_sql_name(kind).unwrap();
        let domain = uqa_sql::ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(range.clone()),
        };
        for ty in [range, domain, uqa_sql::ColumnType::Text] {
            let expected = if ty == uqa_sql::ColumnType::Text {
                2
            } else {
                0
            };
            for budget in [1, 64 * 1024] {
                for descending in [false, true] {
                    for reverse in [false, true] {
                        let template = AggregateAccumulatorTemplate::builtin("mode", Some(&ty))
                            .with_ordering_types(vec![Some(ty.clone())]);
                        let mut accumulator = template.instantiate(budget).unwrap();
                        let mut inputs = [0, 1, 2, 0, 1, 2, 2];
                        if reverse {
                            inputs.reverse();
                        }
                        for value in inputs
                            .iter()
                            .map(|&index| &values[index])
                            .chain([&Value::Null])
                        {
                            accumulator
                                .observe_with_sort_keys(
                                    value,
                                    vec![super::super::ordering::AggregateSortKey::directed(
                                        value.clone(),
                                        descending,
                                    )],
                                    None,
                                )
                                .unwrap();
                        }
                        assert_eq!(!accumulator.values.runs.is_empty(), budget == 1);
                        let actual = aggregate_value("mode", &accumulator, None).unwrap();
                        assert!(uqa_sql::expr::equal_selected_values_with_catalog(
                            &actual,
                            &values[expected],
                            Some(&ty),
                            &uqa_core::memory::ProductionControl::uncontrolled(),
                            None,
                        )
                        .unwrap(), "{kind}: {ty:?}, budget={budget}, descending={descending}, reverse={reverse}");
                    }
                }
            }
        }
    }
}

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
            let mut accumulator = template.instantiate(budget).unwrap();
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
