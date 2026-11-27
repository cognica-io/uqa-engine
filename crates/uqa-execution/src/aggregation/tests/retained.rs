//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::DatumValue;

#[test]
fn numeric_and_boolean_transitions_observe_physical_fields_but_count_keeps_them_lazy() {
    let mut sum = AggregateAccumulator::builtin("sum");
    for value in [1_i32, 2, 3] {
        sum.observe(&Value::Datum(DatumValue::new(
            23,
            0,
            value.to_le_bytes().to_vec(),
        )))
        .unwrap();
    }
    assert_eq!(aggregate_value("sum", &sum, None).unwrap(), Value::Int(6));
    let mut all = AggregateAccumulator::builtin("bool_and");
    let mut any = AggregateAccumulator::builtin("bool_or");
    for value in [1, 0] {
        let value = Value::Datum(DatumValue::new(16, 0, vec![value]));
        all.observe(&value).unwrap();
        any.observe(&value).unwrap();
    }
    assert_eq!(
        aggregate_value("bool_and", &all, None).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        aggregate_value("bool_or", &any, None).unwrap(),
        Value::Bool(true)
    );
    let malformed = Value::Datum(DatumValue::new(23, u32::MAX, vec![]));
    let mut count = AggregateAccumulator::builtin("count");
    count.observe(&malformed).unwrap();
    assert_eq!(
        aggregate_value("count", &count, None).unwrap(),
        Value::Int(1)
    );
    let error = sum.observe(&malformed).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(aggregate_value("sum", &sum, None).unwrap(), Value::Int(6));
}

#[test]
fn string_aggregation_observes_the_first_delimiter_in_its_transition() {
    let malformed = Value::Datum(DatumValue::new(25, u32::MAX, vec![]));
    let pair = Value::List(vec![Value::Str("one".into()), malformed.clone()]);
    let mut state = super::super::string_agg::Transition::default();
    assert_eq!(
        state.accept(&pair, None, |_| {}).unwrap_err().sqlstate(),
        Some("XX001")
    );
    let null = Value::List(vec![Value::Null, malformed]);
    state
        .accept(&null, None, |_| panic!("NULL row emitted bytes"))
        .unwrap();
}
