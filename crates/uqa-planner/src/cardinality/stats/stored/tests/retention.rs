//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cardinality::stats::columns::Builder;
use uqa_core::memory::{Produced, ProductionControl};

#[test]
fn stored_statistics_failure_closes_and_retires_the_complete_prefix() {
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let mut input = StoredColumnStatistics::new(&memory, &token, &token);
    input.insert(row()).unwrap();
    assert!(memory.used() > 0);
    assert!(matches!(
        input.insert(StoredColumnStats {
            row_count: -1,
            ..row()
        }),
        Err(StoredColumnStatsError::Invalid(_))
    ));
    assert_eq!(memory.used(), 0);
    assert!(matches!(
        input.insert(row()),
        Err(StoredColumnStatsError::Finished)
    ));
    assert!(matches!(
        input.finish(),
        Err(StoredColumnStatsError::Finished)
    ));

    let mut input = StoredColumnStatistics::new(&memory, &token, &token);
    input.insert(row()).unwrap();
    let block = memory.reserve(65536 - memory.used()).unwrap();
    assert!(input.insert(row()).is_err());
    drop(block);
    assert_eq!(memory.used(), 0);
    assert!(matches!(
        input.finish(),
        Err(StoredColumnStatsError::Finished)
    ));
}

#[test]
fn stored_statistics_input_retains_controls_beyond_the_constructing_scope() {
    let (mut input, memory, cancellation) = {
        let memory = MemoryBudget::new(65536);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        (
            StoredColumnStatistics::new(&memory, &original, &invoking),
            memory.clone(),
            original.clone(),
        )
    };
    input.insert(row()).unwrap();
    let published = input.finish().unwrap();
    cancellation.cancel();
    assert_eq!(
        published.get("amount").unwrap().row_count,
        u64::try_from(row().row_count).unwrap()
    );
    assert!(memory.used() > 0);
    drop(published);
    assert_eq!(memory.used(), 0);
}

fn fixture(name: &str, index: usize) -> StoredColumnStats<'_> {
    StoredColumnStats {
        column_name: name,
        min_value: Some(r#"{"nested":[["first"],["second"]]}"#),
        histogram_json: if index.is_multiple_of(2) {
            r#"["small"]"#
        } else {
            r#"["a longer replacement",42]"#
        },
        ..row()
    }
}

#[test]
fn stored_statistics_cached_footprint_preserves_replacement_and_capture() {
    let memory = MemoryBudget::new(1024 * 1024);
    let reference_memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let mut input = StoredColumnStatistics::new(&memory, &token, &token);
    let mut reference = Builder::new(ProductionControl::new(&reference_memory, &token, &token));
    for (index, name) in ["a", "longer column", "a", "third", "longer column"]
        .into_iter()
        .enumerate()
    {
        let row = fixture(name, index);
        input.insert(row).unwrap();
        reference
            .insert(
                name,
                Produced::from(
                    row.decode_with_control(&reference_memory, &token, &token)
                        .unwrap(),
                ),
            )
            .unwrap();
    }
    let actual = input.finish().unwrap();
    let reference = reference.finish().unwrap();
    assert_eq!(actual.len(), reference.len());
    for ((actual_name, actual_row), (expected_name, expected_row)) in
        actual.iter().zip(reference.iter())
    {
        assert_eq!(actual_name, expected_name);
        assert_eq!(actual_row.row_count, expected_row.row_count);
        assert_eq!(actual_row.min_value, expected_row.min_value);
        assert_eq!(actual_row.histogram, expected_row.histogram);
    }
    let actual_capture = MemoryBudget::new(1024 * 1024);
    let reference_capture = MemoryBudget::new(1024 * 1024);
    let held_actual = actual
        .capture_with_control(&ProductionControl::new(&actual_capture, &token, &token))
        .unwrap();
    let held_reference = reference
        .capture_with_control(&ProductionControl::new(&reference_capture, &token, &token))
        .unwrap();
    assert_eq!(actual_capture.used(), reference_capture.used());
    drop((actual, reference, held_actual, held_reference));
    for memory in [memory, reference_memory, actual_capture, reference_capture] {
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_statistics_publication_does_not_rescan_retained_native_values() {
    for count in [32, 320] {
        let memory = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let mut input = StoredColumnStatistics::new(&memory, &token, &token);
        let mut reference = Builder::new(ProductionControl::new(&memory, &token, &token));
        for index in 0..count {
            let name = format!("column_{index}");
            let row = fixture(&name, index);
            input.insert(row).unwrap();
            reference
                .insert(
                    &name,
                    Produced::from(row.decode_with_control(&memory, &token, &token).unwrap()),
                )
                .unwrap();
        }
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(input.finish().unwrap());
        });
        assert_eq!(
            allocations.count_total, 1,
            "only the immutable shared header is allocated"
        );
        let mut original = None;
        let scan = allocation_counter::measure(|| {
            original = Some(reference.finish().unwrap());
        });
        assert!(
            scan.count_total > allocations.count_total,
            "the original traversal allocates native nested-value cursors"
        );
        assert_eq!(output.as_ref().unwrap().len(), count);
        drop((output, original));
        assert_eq!(memory.used(), 0);
    }
}
