//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::ordering::AggregateSortKey;
use uqa_sql::ColumnType;

#[derive(Default)]
struct ArgumentCount(i64);

impl SQLAggregateState for ArgumentCount {
    fn observe(&mut self, values: &[Value]) -> Result<(), SQLError> {
        assert_eq!(values.len(), 2);
        self.0 += 1;
        Ok(())
    }
    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Int(self.0))
    }
}

#[test]
fn aggregate_argument_ranges_preserve_types_across_spill_and_restore() {
    for kind in ["numrange", "nummultirange"] {
        let values = ["[1.0,3.00)", "[1.00,3.0)", "[4,5)"].map(|value| {
            let text = if kind == "nummultirange" {
                format!("{{{value}}}")
            } else {
                value.into()
            };
            cast_value(&Value::Str(text), kind).unwrap()
        });
        let range = ColumnType::from_sql_name(kind).unwrap();
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "argument_range".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(range.clone()),
        };
        for ty in [range, domain, ColumnType::Text] {
            for range_first in [false, true] {
                for ordered in [false, true] {
                    for budget in [1, 64 * 1024] {
                        verify_arguments(&values, &ty, range_first, ordered, budget);
                    }
                }
            }
        }
    }
}

fn verify_arguments(
    values: &[Value; 3],
    ty: &ColumnType,
    range_first: bool,
    ordered: bool,
    budget: usize,
) {
    let expected = if *ty == ColumnType::Text { 7 } else { 4 };
    let mut types = if ordered {
        vec![Some(ColumnType::Integer)]
    } else {
        vec![]
    };
    let mut arguments = [Some(ty.clone()), Some(ColumnType::Integer)];
    if !range_first {
        arguments.reverse();
    }
    types.extend(arguments);
    let template = AggregateAccumulatorTemplate::registered(Arc::new(ArgumentCount::default))
        .with_distinct_arguments(2)
        .with_ordering_types(types);
    let mut accumulator = template.instantiate(budget).unwrap();
    let inputs = [
        (0, Some(1)),
        (1, Some(1)),
        (2, Some(1)),
        (0, Some(2)),
        (1, Some(2)),
        (0, None),
        (1, None),
    ];
    for _ in 0..(AGGREGATE_MERGE_FAN_IN + 1).div_ceil(inputs.len()) {
        for (range, number) in inputs {
            let number = number.map_or(Value::Null, Value::Int);
            let keys = if ordered {
                vec![AggregateSortKey::directed(number.clone(), true)]
            } else {
                vec![]
            };
            let mut arguments = vec![values[range].clone(), number];
            if !range_first {
                arguments.reverse();
            }
            accumulator
                .distinct
                .insert(&Value::List(arguments), keys, None)
                .unwrap();
        }
    }
    assert_eq!(!accumulator.distinct.values.runs.is_empty(), budget == 1);
    accumulator.distinct.argument_count = None;
    accumulator.distinct.values.key_types = None;
    template.restore_comparison(&mut accumulator);
    let mut observed = 0;
    accumulator
        .distinct
        .for_each(None, |value| {
            let Value::List(arguments) = value else {
                panic!("argument tuple");
            };
            assert_eq!(arguments.len(), 2);
            let range = &arguments[usize::from(!range_first)];
            assert!(values
                .iter()
                .any(|input| input.has_same_representation(range)));
            observed += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        observed, expected,
        "{ty:?}, range_first={range_first}, ordered={ordered}, budget={budget}"
    );
    assert_eq!(
        aggregate_value("argument_count", &accumulator, None).unwrap(),
        Value::Int(expected)
    );
}
