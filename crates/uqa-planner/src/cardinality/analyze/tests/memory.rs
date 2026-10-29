//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn counts(comparable: u64) -> ColumnObservationCounts {
    ColumnObservationCounts {
        nulls: 0,
        omitted: 0,
        comparable,
    }
}

#[test]
fn distribution_rejects_a_large_copy_before_allocating_its_payload() {
    let value = Value::Str("x".repeat(4 * 1024 * 1024));
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(collect_statistics_with_control(
            1,
            &[counts(1)],
            [Ok((0, value))],
            101,
            10,
            ProductionControl::new(&budget, &token, &token),
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 0);
}

#[test]
fn column_scratch_is_admitted_before_consuming_the_population() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let mut read = false;
    let population = std::iter::from_fn(|| {
        read = true;
        None
    });
    let error = collect_named_statistics(
        0,
        std::iter::repeat_n(("column", counts(0)), 100_000),
        population,
        101,
        10,
        ProductionControl::new(&budget, &token, &token),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(!read);
    assert_eq!(budget.used(), 0);
}

#[test]
fn interleaved_columns_match_independent_population_statistics() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let left = (0..128)
        .map(|i| Value::Str(format!("{:04}", i / 3)))
        .collect::<Vec<_>>();
    let right = (0..128)
        .map(|i| Value::List(vec![Value::Int(i / 11)]))
        .collect::<Vec<_>>();
    let expected = [reference(&left, 0, 0), reference(&right, 0, 0)];
    let values = left
        .into_iter()
        .zip(right)
        .flat_map(|(left, right)| [Ok((0, left)), Ok((1, right))]);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(
            collect_statistics_with_control(
                128,
                &[counts(128), counts(0)],
                values,
                101,
                10,
                ProductionControl::new(&budget, &token, &token),
            )
            .unwrap(),
        );
    });
    let result = result.unwrap();
    for (actual, expected) in result.iter().zip(&expected) {
        assert_stats(actual, expected);
    }
    assert_eq!(result.reserved_bytes(), budget.used());
    // The population buffers existed before the measurement and were consumed in it.
    assert!(allocation.bytes_max < 128 * 1024, "{allocation:?}");
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn maximum_and_losing_common_values_release_their_individual_leases() {
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let first = Value::Str("a".repeat(1024 * 1024));
    let mut distribution = Distribution::new(2, counts(2), 2, 0, control);
    distribution.observe(&first).unwrap();
    let original = budget.used();
    distribution.observe(&Value::Str("z".into())).unwrap();
    assert!(budget.used() + 1024 * 1024 < original);
    drop(distribution);
    assert_eq!(budget.used(), 0);

    let first = Value::List(vec![Value::Str("a".repeat(1024 * 1024))]);
    let second = Value::List(vec![Value::Str("b".into())]);
    let third = Value::List(vec![Value::Str("c".into())]);
    let mut distribution = Distribution::new(5, counts(0), 2, 1, control);
    distribution.observe(&first).unwrap();
    for _ in 0..3 {
        distribution.observe(&second).unwrap();
    }
    assert!(budget.used() >= 1024 * 1024);
    distribution.observe(&third).unwrap();
    assert!(budget.used() < 8192);
    let output = distribution.finish().unwrap();
    assert_eq!(output.mcv_values, [second]);
    assert_eq!(output.mcv_frequencies, [0.6]);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn distribution_errors_and_unwind_release_partial_construction() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for values in [
        vec![
            Ok((0, Value::Str("a".repeat(1024)))),
            Err(SQLError::Internal("input error".into())),
        ],
        vec![Ok((0, Value::Int(2))), Ok((0, Value::Int(1)))],
        vec![Ok((0, Value::Int(2))), Ok((1, Value::Int(3)))],
        vec![Ok((0, Value::Int(2)))],
    ] {
        assert!(
            collect_statistics_with_control(2, &[counts(2)], values, 101, 10, control).is_err()
        );
        assert_eq!(budget.used(), 0);
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let values = (0..2).map(|i| {
            assert_eq!(i, 0, "test input unwind");
            Ok((0, Value::Str("a".repeat(1024))))
        });
        let _ = collect_statistics_with_control(2, &[counts(2)], values, 101, 10, control);
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn distribution_cancellation_observes_both_owners_and_preserves_input_errors() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let cancel = if cancel_original {
            &original
        } else {
            &invoking
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let values = (0..2).map(|i| {
            if i == 1 {
                cancel.cancel();
            }
            Ok((0, Value::Str(format!("{i:01024}"))))
        });
        let error =
            collect_statistics_with_control(2, &[counts(2)], values, 101, 10, control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let values = std::iter::once_with(|| {
        token.cancel();
        Err(SQLError::Internal("original input error".into()))
    });
    let error = collect_statistics_with_control(
        1,
        &[counts(1)],
        values,
        101,
        10,
        ProductionControl::new(&budget, &token, &token),
    )
    .unwrap_err();
    assert!(error.to_string().contains("original input error"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn named_statistics_release_duplicate_outputs_and_retain_only_the_final_generation() {
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(
            collect_named_statistics(
                1,
                [("value", counts(1)), ("value", counts(1))],
                [
                    Ok((0, Value::Str("a".repeat(1024 * 1024)))),
                    Ok((1, Value::Int(7))),
                ],
                101,
                10,
                control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output["value"].min_value, Some(Value::Int(7)));
    assert!(budget.used() < 8192);
    // Arc's two reference counters are explicitly outside the owned payload allowance.
    assert_eq!(
        allocation.bytes_current as usize,
        budget.used() + 2 * size_of::<usize>()
    );
    let pointer = &raw const output["value"];
    let mut readers = None;
    assert_eq!(
        allocation_counter::measure(|| {
            readers = Some([output.clone(), output.clone()]);
        })
        .count_total,
        0
    );
    drop(output);
    let readers = readers.unwrap();
    assert_eq!(&raw const readers[0]["value"], pointer);
    assert!(budget.used() > 0);
    drop(readers);
    assert_eq!(budget.used(), 0);
}

#[test]
fn named_statistics_partial_replacement_preserves_readers_and_parent_admission() {
    let parent = MemoryBudget::new(1024 * 1024);
    let budget = parent.child(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let before = collect_named_statistics(
        1,
        [("a", counts(1)), ("b", counts(1))],
        [Ok((0, Value::Int(1))), Ok((1, Value::Int(2)))],
        101,
        10,
        control,
    )
    .unwrap();
    let replacement = collect_named_statistics(
        1,
        [("b", counts(1))],
        [Ok((0, Value::Int(7)))],
        101,
        10,
        control,
    )
    .unwrap();
    let used = budget.used();
    let full = parent.reserve(parent.available()).unwrap();
    assert_eq!(
        before
            .with_replacements(&replacement, control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), used);
    drop(full);
    let changed = before.with_replacements(&replacement, control).unwrap();
    assert_eq!(changed["a"].min_value, Some(Value::Int(1)));
    assert_eq!(changed["b"].min_value, Some(Value::Int(7)));
    assert_eq!(before["b"].min_value, Some(Value::Int(2)));
    drop((before, replacement));
    assert!(budget.used() > 0);
    assert_eq!(parent.used(), budget.used());
    drop(changed);
    assert_eq!(parent.used(), 0);
}
