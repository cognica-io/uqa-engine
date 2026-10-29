//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod memory;
use std::collections::{BTreeMap, BTreeSet};

fn comparable(value: &Value) -> bool {
    matches!(
        value,
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

// Preserve the pre-spill distribution contract independently of the streaming fold: exact sets, a stable population sort and a frequency map.
fn reference(values: &[Value], nulls: u64, omitted: u64) -> ColumnStats {
    let row_count = values.len() as u64 + nulls + omitted;
    let distinct = values.iter().collect::<BTreeSet<_>>().len() as u64 + omitted;
    let mut ordered = values
        .iter()
        .filter(|value| comparable(value))
        .collect::<Vec<_>>();
    ordered.sort();
    let mut histogram = Vec::new();
    if !ordered.is_empty() {
        let buckets = 100.min(ordered.len());
        histogram.push(ordered[0].clone());
        for bucket in 1..buckets {
            let value = ordered[bucket * ordered.len() / buckets];
            if histogram.last() != Some(value) {
                histogram.push(value.clone());
            }
        }
        let last = ordered[ordered.len() - 1];
        if ordered.len() == 1 || histogram.last() != Some(last) {
            histogram.push(last.clone());
        }
    }
    let mut counts = BTreeMap::<&Value, u64>::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let mut common = Vec::new();
    let mut frequencies = Vec::new();
    for (value, count) in counts.into_iter().take(10) {
        let frequency = count as f64 / row_count as f64;
        if frequency > 1.0 / distinct as f64 {
            common.push(value.clone());
            frequencies.push(frequency);
        }
    }
    ColumnStats {
        distinct_count: distinct,
        null_count: nulls,
        min_value: ordered.first().map(|value| (*value).clone()),
        max_value: ordered.last().map(|value| (*value).clone()),
        row_count,
        histogram,
        mcv_values: common,
        mcv_frequencies: frequencies,
    }
}

fn assert_stats(actual: &ColumnStats, expected: &ColumnStats) {
    assert_eq!(actual.distinct_count, expected.distinct_count);
    assert_eq!(actual.null_count, expected.null_count);
    assert_eq!(actual.row_count, expected.row_count);
    assert_eq!(actual.min_value, expected.min_value);
    assert_eq!(actual.max_value, expected.max_value);
    assert_eq!(actual.histogram, expected.histogram);
    assert_eq!(actual.mcv_values, expected.mcv_values);
    assert_eq!(
        actual
            .mcv_frequencies
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        expected
            .mcv_frequencies
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn sorted_statistics_preserve_complete_population_distributions() {
    let populations = [
        Vec::new(),
        vec![Value::Int(7)],
        vec![Value::List(vec![Value::Int(2)]); 13],
        (0..4096).map(|n| Value::Int(n % 173)).collect(),
        (0..1024)
            .map(|n| match n % 5 {
                0 => Value::Bool(n % 2 == 0),
                1 => Value::Int(n % 11),
                2 => Value::Float(if n % 2 == 0 { -0.0 } else { 0.0 }),
                3 => Value::Str(format!("value-{}", n % 37)),
                _ => Value::List(vec![Value::Int(n % 23)]),
            })
            .collect(),
    ];
    for mut values in populations {
        for (nulls, omitted) in [(0, 0), (19, 3)] {
            let expected = reference(&values, nulls, omitted);
            let counts = [ColumnObservationCounts {
                nulls,
                omitted,
                comparable: values.iter().filter(|value| comparable(value)).count() as u64,
            }];
            values.sort();
            let actual = collect_statistics(
                expected.row_count,
                &counts,
                values.iter().cloned().map(|value| Ok((0, value))),
                101,
                10,
            )
            .unwrap();
            assert_stats(&actual[0], &expected);
            let budget = uqa_core::memory::MemoryBudget::new(1024 * 1024);
            let token = uqa_core::CancellationToken::new();
            let actual = collect_statistics_with_control(
                expected.row_count,
                &counts,
                values.iter().cloned().map(|value| Ok((0, value))),
                101,
                10,
                ProductionControl::new(&budget, &token, &token),
            )
            .unwrap();
            assert_stats(&actual[0], &expected);
            assert_eq!(actual.reserved_bytes(), budget.used());
            drop(actual);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn sorted_statistics_reject_incomplete_and_out_of_order_input() {
    let counts = [ColumnObservationCounts {
        nulls: 0,
        omitted: 0,
        comparable: 2,
    }];
    let incomplete = collect_statistics(2, &counts, [Ok((0, Value::Int(1)))], 101, 10);
    assert!(incomplete.unwrap_err().to_string().contains("incomplete"));
    let reversed = collect_statistics(
        2,
        &counts,
        [Ok((0, Value::Int(2))), Ok((0, Value::Int(1)))],
        101,
        10,
    );
    assert!(reversed.unwrap_err().to_string().contains("not ordered"));
}

#[test]
fn sorted_statistics_keep_only_bounded_distribution_outputs() {
    let counts = [ColumnObservationCounts {
        nulls: 0,
        omitted: 0,
        comparable: 65_536,
    }];
    let allocated = allocation_counter::measure(|| {
        let stats = collect_statistics(
            65_536,
            &counts,
            (0..65_536).map(|n| Ok((0, Value::Int(n)))),
            101,
            10,
        )
        .unwrap();
        assert_eq!(stats[0].distinct_count, 65_536);
        assert_eq!(stats[0].histogram.len(), 101);
        assert!(stats[0].mcv_values.is_empty());
    });
    assert!(allocated.bytes_max < 128 * 1024, "{allocated:?}");
}
