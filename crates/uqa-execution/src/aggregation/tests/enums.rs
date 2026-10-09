//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::ordering::AggregateSortKey;
use uqa_core::{DatumValue, EnumLabelKey, EnumValue};
use uqa_sql::ast::{ColumnType, EnumTypeReference};
use uqa_sql::expr::enums::{EnumLabelCatalog, EnumTypeComparisonStates, EnumTypeLabels};

#[derive(Default)]
struct Catalog(EnumTypeComparisonStates);

impl EnumLabelCatalog for Catalog {
    fn enum_type_comparison_states(&self) -> Option<&EnumTypeComparisonStates> {
        Some(&self.0)
    }
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(matches!(oid, 16_384 | 16_400).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: Vec::new(),
            })
        }))
    }
    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>, SQLError> {
        Ok(match oid {
            2 => Some((16_384, 0)),
            5 => Some((16_384, 1)),
            4 => Some((16_384, 2)),
            11 => Some((16_400, 0)),
            8 => Some((16_400, 1)),
            _ => None,
        })
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("aggregate comparison must not repeat enum input")
    }
    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok(Some(format!("enum_{oid}")))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

fn enum_type() -> ColumnType {
    ColumnType::Enum(EnumTypeReference {
        schema: "public".into(),
        name: "declared_enum".into(),
        oid: 16_400,
        array_oid: 16_401,
    })
}

fn physical(oid: u32) -> Value {
    Value::Datum(DatumValue::new(16_400, 0, oid.to_le_bytes().to_vec()))
}

fn shaped(shape: usize, value: Value) -> Value {
    match shape {
        0 => value,
        1 => Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
        _ => Value::Record(vec![("e".into(), value)].into()),
    }
}

#[test]
fn extrema_keep_transition_enum_state_across_groups_and_partial_merges() {
    for name in ["min", "max"] {
        let catalog = Catalog::default();
        let template = AggregateAccumulatorTemplate::builtin(name, Some(&enum_type()));
        let mut first = template.instantiate(1);
        first
            .observe_with_enum_catalog(&physical(5), Some(&catalog))
            .unwrap();
        let mut second = template.instantiate(1);
        second
            .observe_with_enum_catalog(&physical(4), Some(&catalog))
            .unwrap();
        super::super::partial_state::merge_accumulators(&mut first, second, Some(&catalog))
            .unwrap();
        assert_eq!(
            aggregate_value(name, &first, Some(&catalog)).unwrap(),
            physical(if name == "min" { 5 } else { 4 })
        );

        // Recreating the group or window frame does not recreate the transition function's cache.
        let mut next = template.instantiate(1);
        next.observe_with_enum_catalog(&physical(11), Some(&catalog))
            .unwrap();
        let error = next
            .observe_with_enum_catalog(&physical(8), Some(&catalog))
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert!(error.to_string().contains("enum_16384"));

        let mut restored = AggregateAccumulator::builtin(name);
        template.restore_comparison(&mut restored);
        restored
            .observe_with_enum_catalog(&physical(11), Some(&catalog))
            .unwrap();
        assert_eq!(
            restored
                .observe_with_enum_catalog(&physical(8), Some(&catalog))
                .unwrap_err()
                .sqlstate(),
            Some("XX000")
        );
    }
}

#[derive(Default)]
struct Values(Vec<Value>);

impl SQLAggregateState for Values {
    fn observe(&mut self, values: &[Value]) -> Result<(), SQLError> {
        self.0.extend_from_slice(values);
        Ok(())
    }
    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::List(self.0.clone()))
    }
}

#[test]
fn aggregate_sort_support_keeps_its_enum_state_when_the_group_changes() {
    for budget in [1, 4096] {
        for kind in 0..3 {
            let catalog = Catalog::default();
            let template = match kind {
                0 => AggregateAccumulatorTemplate::builtin("array_agg", None),
                1 => AggregateAccumulatorTemplate::builtin("count", Some(&enum_type())),
                _ => AggregateAccumulatorTemplate::registered(Arc::new(Values::default)),
            }
            .with_ordering(1);
            for (group, oids) in [[5, 4], [11, 8]].into_iter().enumerate() {
                let mut accumulator = template.instantiate(budget);
                let result = (|| {
                    for oid in oids {
                        let value = physical(oid);
                        let keys = vec![AggregateSortKey::ascending(value.clone())];
                        match kind {
                            0 => accumulator.observe_with_sort_keys(
                                &Value::Int(1),
                                keys,
                                Some(&catalog),
                            )?,
                            1 => accumulator
                                .distinct
                                .insert(&value, Vec::new(), Some(&catalog))?,
                            _ => accumulator.observe_registered(
                                vec![Value::Int(1)],
                                keys,
                                Some(&catalog),
                            )?,
                        }
                    }
                    aggregate_value(
                        if kind == 0 { "array_agg" } else { "count" },
                        &accumulator,
                        Some(&catalog),
                    )
                })();
                if group == 0 {
                    result.unwrap();
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(error.sqlstate(), Some("XX000"));
                    assert!(error.to_string().contains("enum_16384"));
                }
            }
        }
    }
}

#[test]
fn ordered_and_distinct_aggregates_keep_enum_order_across_memory_and_merge_runs() {
    for budget in [1, 4096] {
        for shape in 0..3 {
            let catalog = Catalog::default();
            let mut values = AggregateValueBuffer::new(budget);
            let mut registered = RegisteredAggregateBuffer::new(budget);
            let mut distinct = DistinctTracker::new(budget);
            for oid in [4, 5, 2].into_iter().cycle().take(18) {
                let key = shaped(shape, physical(oid));
                let keys = vec![AggregateSortKey::ascending(key.clone())];
                values
                    .push(Value::Int(i64::from(oid)), keys.clone(), Some(&catalog))
                    .unwrap();
                registered
                    .push(vec![Value::Int(i64::from(oid))], keys, Some(&catalog))
                    .unwrap();
                distinct.insert(&key, Vec::new(), Some(&catalog)).unwrap();
            }
            let expected: Vec<_> = [2, 5, 4]
                .into_iter()
                .flat_map(|oid| std::iter::repeat_n(Value::Int(oid), 6))
                .collect();
            assert_eq!(values.ordered_values(Some(&catalog)).unwrap(), expected);
            let mut state = Values::default();
            registered
                .observe_ordered_into(&mut state, Some(&catalog))
                .unwrap();
            assert_eq!(state.0, expected);
            let mut unique = Vec::new();
            distinct
                .for_each(Some(&catalog), |value| {
                    unique.push(value.clone());
                    Ok(())
                })
                .unwrap();
            assert_eq!(unique, [2, 5, 4].map(|oid| shaped(shape, physical(oid))));
            assert_eq!(!values.runs.is_empty(), budget == 1);
        }
    }
}

#[test]
fn aggregate_distinct_and_mode_group_admitted_and_physical_enum_identity() {
    for budget in [1, 4096] {
        for shape in 0..3 {
            let catalog = Catalog::default();
            let admitted = Value::Enum(
                EnumValue::new(16_400, EnumLabelKey::initial(1).unwrap().remove(0))
                    .with_label_oid(Some(5)),
            );
            let mut mode = AggregateAccumulator::builtin_with_budget("mode", budget);
            let mut distinct = DistinctTracker::new(budget);
            for value in [physical(2), admitted, physical(5)] {
                let value = shaped(shape, value);
                mode.observe_with_sort_keys(
                    &value,
                    vec![AggregateSortKey::ascending(value.clone())],
                    Some(&catalog),
                )
                .unwrap();
                distinct.insert(&value, Vec::new(), Some(&catalog)).unwrap();
            }
            let chosen = aggregate_value("mode", &mode, Some(&catalog)).unwrap();
            assert!(uqa_sql::expr::equal_typed_values_with_enum_catalog(
                &chosen,
                &shaped(shape, physical(5)),
                &uqa_core::memory::ProductionControl::uncontrolled(),
                Some(&catalog)
            )
            .unwrap());
            let mut count = 0;
            distinct
                .for_each(Some(&catalog), |_| {
                    count += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(count, 2);
        }
    }
}

#[test]
fn aggregate_enum_order_does_not_validate_equal_or_even_oids() {
    for budget in [1, 4096] {
        for shape in 0..3 {
            for oids in [[1, 1], [6, 10], [1, 3]] {
                let catalog = Catalog::default();
                let mut distinct = DistinctTracker::new(budget);
                let result = (|| {
                    for oid in oids {
                        distinct.insert(
                            &shaped(shape, physical(oid)),
                            Vec::new(),
                            Some(&catalog),
                        )?;
                    }
                    let mut count = 0;
                    distinct.for_each(Some(&catalog), |_| {
                        count += 1;
                        Ok(())
                    })?;
                    Ok::<_, SQLError>(count)
                })();
                if oids == [1, 3] {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("22P03"));
                } else {
                    assert_eq!(result.unwrap(), if oids == [1, 1] { 1 } else { 2 });
                }
            }
        }
    }
}
