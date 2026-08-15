//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::ArrayValue;

#[test]
fn aggregate_spill_record_reader_rejects_oversized_and_truncated_records() {
    let mut oversized = std::io::Cursor::new(b"12345\n".to_vec());
    let error =
        read_bounded_json_spill_record(&mut oversized, 5, "test aggregate spill row").unwrap_err();
    assert!(error.to_string().contains("exceeds recorded maximum"));

    let mut truncated = std::io::Cursor::new(b"12345".to_vec());
    let error =
        read_bounded_json_spill_record(&mut truncated, 6, "test aggregate spill row").unwrap_err();
    assert!(error.to_string().contains("missing record delimiter"));
}

#[test]
fn streaming_aggregate_does_not_retain_or_spill_inputs() {
    let mut accumulator = AggregateAccumulator::builtin("sum");
    let end = 4097_i64;
    for value in 0..end {
        accumulator.observe(&Value::Int(value)).unwrap();
    }

    assert!(accumulator.values.rows.is_empty());
    assert!(accumulator.values.runs.is_empty());
    assert_eq!(
        aggregate_value("sum", &accumulator).unwrap(),
        Value::Int(end * (end - 1) / 2)
    );
}

#[test]
fn collection_aggregate_still_retains_inputs() {
    let mut accumulator = AggregateAccumulator::builtin("array_agg");
    accumulator.observe(&Value::Int(7)).unwrap();

    assert_eq!(accumulator.count, 0);
    assert_eq!(accumulator.decimal_sum, None);
    assert_eq!(accumulator.min, None);
    assert_eq!(accumulator.max, None);
    assert_eq!(accumulator.values.rows.len(), 1);
    assert_eq!(
        aggregate_value("array_agg", &accumulator).unwrap(),
        Value::Array(ArrayValue::try_new(vec![Value::Int(7)]).expect("one-dimensional array"))
    );
}

#[test]
fn ordered_aggregate_buffers_reject_sequence_overflow_without_appending() {
    let mut builtin = AggregateValueBuffer::new(1024);
    builtin.next_sequence = u64::MAX;
    let error = builtin.push(Value::Int(1), Vec::new()).unwrap_err();
    assert!(error.to_string().contains("sequence overflow"));
    assert!(builtin.rows.is_empty());

    let mut registered = RegisteredAggregateBuffer::new(1024);
    registered.next_sequence = u64::MAX;
    let error = registered
        .push(vec![Value::Int(1)], Vec::new())
        .unwrap_err();
    assert!(error.to_string().contains("sequence overflow"));
    assert!(registered.rows.is_empty());
}

#[test]
fn streaming_aggregate_counts_report_overflow() {
    let mut count = AggregateAccumulator::builtin("count");
    count.count = u64::MAX;
    let error = count.observe(&Value::Int(1)).unwrap_err();
    assert!(error.to_string().contains("count overflow"));

    let mut statistics = AggregateAccumulator::builtin("stddev_pop");
    statistics.statistics_count = u64::MAX;
    let error = statistics.observe(&Value::Int(1)).unwrap_err();
    assert!(error.to_string().contains("count overflow"));
}

#[test]
fn tiny_budget_collection_aggregate_spills_and_merge_streams_exact_order() {
    let mut accumulator = AggregateAccumulator::builtin_with_budget("array_agg", 2);
    for value in (0..512_i64).rev() {
        accumulator
            .observe_with_sort_keys(&Value::Int(value), vec![(Value::Int(value), false)])
            .unwrap();
    }

    assert!(!accumulator.values.runs.is_empty());
    assert!(accumulator.values.runs.len() < AGGREGATE_MERGE_FAN_IN);
    assert!(accumulator.values.memory_bytes <= accumulator.values.budget_bytes);
    let expected = Value::Array(
        ArrayValue::try_new((0..512_i64).map(Value::Int).collect()).expect("one-dimensional array"),
    );
    assert_eq!(
        aggregate_value("array_agg", &accumulator).unwrap(),
        expected
    );
}

#[test]
fn collection_aggregate_rejects_a_spill_record_larger_than_writer_metadata() {
    let mut values = AggregateValueBuffer::new(1);
    values
        .push(Value::Int(1), vec![(Value::Int(1), false)])
        .unwrap();
    let run = values.runs.first_mut().unwrap();
    run.file.as_file_mut().seek(SeekFrom::End(0)).unwrap();
    run.file
        .as_file_mut()
        .write_all(&vec![b'x'; run.max_record_bytes])
        .unwrap();
    run.file.as_file_mut().write_all(b"\n").unwrap();
    run.file.as_file_mut().flush().unwrap();

    let error = values.ordered_values().unwrap_err();
    assert!(error.to_string().contains("exceeds recorded maximum"));
}

#[test]
fn tiny_budget_distinct_tracker_migrates_to_disk() {
    let mut tracker = DistinctTracker::new(1);
    assert!(tracker.insert(&Value::Str("alpha".into())).unwrap());
    assert!(tracker.disk.is_some());
    assert!(tracker.memory.is_empty());
    assert!(!tracker.insert(&Value::Str("alpha".into())).unwrap());
    assert!(tracker.insert(&Value::Str("beta".into())).unwrap());
}

#[test]
fn distinct_tracker_uses_value_numeric_equality_without_string_keys() {
    let mut tracker = DistinctTracker::new(1024);
    assert!(tracker.insert(&Value::Int(1)).unwrap());
    assert!(!tracker.insert(&Value::Float(1.0)).unwrap());
    assert!(!tracker
        .insert(&Value::Decimal(DecimalValue::parse("1.00").unwrap()))
        .unwrap());
}

#[test]
fn distinct_tracker_rejects_a_spill_record_larger_than_writer_metadata() {
    let mut tracker = DistinctTracker::new(1);
    assert!(tracker.insert(&Value::Str("alpha".into())).unwrap());
    let file = tracker.disk.as_mut().unwrap().as_file_mut();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(&vec![b'x'; tracker.max_disk_record_bytes])
        .unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();

    let error = tracker.insert(&Value::Str("missing".into())).unwrap_err();
    assert!(error.to_string().contains("exceeds recorded maximum"));
}

#[test]
fn builtins_only_update_state_used_by_their_finalizer() {
    let mut count = AggregateAccumulator::builtin("count");
    count.observe(&Value::Int(7)).unwrap();
    assert_eq!(count.count, 1);
    assert_eq!(count.decimal_sum, None);
    assert_eq!(count.min, None);
    assert_eq!(count.max, None);

    let mut sum = AggregateAccumulator::builtin("sum");
    sum.observe(&Value::Int(7)).unwrap();
    assert_eq!(sum.count, 1);
    assert_eq!(sum.integer_sum, 7);
    assert_eq!(sum.decimal_sum, None);
    assert_eq!(sum.min, None);
    assert_eq!(sum.max, None);

    let mut min = AggregateAccumulator::builtin("min");
    min.observe(&Value::Int(7)).unwrap();
    assert_eq!(min.count, 0);
    assert_eq!(min.decimal_sum, None);
    assert_eq!(min.min, Some(Value::Int(7)));
    assert_eq!(min.max, None);

    let mut bool_or = AggregateAccumulator::builtin("bool_or");
    bool_or.observe(&Value::Bool(true)).unwrap();
    assert_eq!(bool_or.count, 0);
    assert_eq!(bool_or.bool_and, None);
    assert_eq!(bool_or.bool_or, Some(true));
}

#[test]
fn statistical_aggregate_uses_constant_welford_state() {
    let mut accumulator = AggregateAccumulator::builtin("stddev_pop");
    accumulator.observe(&Value::Int(7)).unwrap();

    assert_eq!(accumulator.statistics_count, 1);
    assert_eq!(accumulator.decimal_sum, None);
    assert_eq!(accumulator.min, None);
    assert_eq!(accumulator.max, None);
    assert!(accumulator.values.rows.is_empty());
    assert!(accumulator.values.runs.is_empty());
}

#[test]
fn integer_sum_stays_exact_beyond_float_precision() {
    let mut accumulator = AggregateAccumulator::builtin("sum");
    accumulator
        .observe(&Value::Int(9_007_199_254_740_992))
        .unwrap();
    accumulator.observe(&Value::Int(1)).unwrap();

    assert_eq!(
        aggregate_value("sum", &accumulator).unwrap(),
        Value::Int(9_007_199_254_740_993)
    );
    assert_eq!(accumulator.decimal_sum, None);
}

#[test]
fn integer_average_promotes_to_float_only_when_finalized_or_mixed() {
    let mut integers = AggregateAccumulator::builtin("avg");
    integers.observe(&Value::Int(2)).unwrap();
    integers.observe(&Value::Int(3)).unwrap();
    assert_eq!(integers.sum, 0.0);
    assert_eq!(
        aggregate_value("avg", &integers).unwrap(),
        Value::Float(2.5)
    );

    integers.observe(&Value::Float(1.5)).unwrap();
    assert_eq!(integers.sum, 6.5);
    assert_eq!(
        aggregate_value("avg", &integers).unwrap(),
        Value::Float(6.5 / 3.0)
    );
}

#[test]
fn decimal_sum_absorbs_integers_observed_before_and_after_it() {
    let mut accumulator = AggregateAccumulator::builtin("sum");
    accumulator.observe(&Value::Int(2)).unwrap();
    accumulator
        .observe(&Value::Decimal(DecimalValue::parse("0.5").unwrap()))
        .unwrap();
    accumulator.observe(&Value::Int(3)).unwrap();

    assert_eq!(
        aggregate_value("sum", &accumulator).unwrap(),
        Value::Decimal(DecimalValue::parse("5.5").unwrap())
    );
    assert_eq!(accumulator.sum, 0.0);
}

#[test]
fn decimal_sum_is_converted_to_float_only_when_a_float_is_observed() {
    let mut accumulator = AggregateAccumulator::builtin("sum");
    accumulator.observe(&Value::Int(2)).unwrap();
    accumulator
        .observe(&Value::Decimal(DecimalValue::parse("0.5").unwrap()))
        .unwrap();
    accumulator.observe(&Value::Int(3)).unwrap();
    assert_eq!(accumulator.sum, 0.0);

    accumulator.observe(&Value::Float(1.25)).unwrap();
    assert_eq!(accumulator.sum, 6.75);
    accumulator
        .observe(&Value::Decimal(DecimalValue::parse("0.25").unwrap()))
        .unwrap();
    accumulator.observe(&Value::Int(1)).unwrap();

    assert_eq!(
        aggregate_value("sum", &accumulator).unwrap(),
        Value::Float(8.0)
    );
}

#[test]
fn aggregate_finalizers_report_integer_width_overflow() {
    let mut count = AggregateAccumulator::builtin("count");
    count.count = i64::MAX as u64 + 1;
    assert!(aggregate_value("count", &count)
        .unwrap_err()
        .to_string()
        .contains("exceeds BIGINT"));

    let mut sum = AggregateAccumulator::builtin("sum");
    sum.count = 1;
    sum.integer_sum = i128::from(i64::MAX) + 1;
    assert!(aggregate_value("sum", &sum)
        .unwrap_err()
        .to_string()
        .contains("exceeds BIGINT"));
}

#[test]
fn percentile_fraction_rejects_missing_and_out_of_range_values() {
    assert!(percentile_fraction(&[]).is_err());
    for fraction in [-0.1, 1.1, f64::NAN] {
        assert!(percentile_fraction(&[ScalarExpr::Literal(Value::Float(fraction))]).is_err());
    }
    assert_eq!(
        percentile_fraction(&[ScalarExpr::Literal(Value::Float(0.25))]).unwrap(),
        0.25
    );
}
