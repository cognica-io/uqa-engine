//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn fixture() -> ColumnStatistics {
    (0..2048)
        .map(|position| {
            (
                format!("column_{position:04}"),
                ColumnStats {
                    distinct_count: 2,
                    row_count: 3,
                    min_value: Some(Value::Str("x".repeat(128))),
                    histogram: vec![Value::Int(-1), Value::Int(4)],
                    mcv_values: vec![Value::Float(-0.0)],
                    mcv_frequencies: vec![0.5],
                    ..Default::default()
                },
            )
        })
        .collect()
}

#[test]
fn statistics_capture_shares_native_roots_without_population_work_or_allocation() {
    let source = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let control = ProductionControl::new(&memory, &token, &token);
    let first = &raw const source["column_0000"];
    let last = &raw const source["column_2047"];
    let mut retained = None;
    let allocation = allocation_counter::measure(|| {
        retained = Some(source.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let retained = retained.unwrap();
    assert!(memory.used() > 2048 * 128);
    assert_eq!(&raw const retained["column_0000"], first);
    assert_eq!(&raw const retained["column_2047"], last);
    let mut changed = source;
    changed.insert("column_0000".into(), ColumnStats::default());
    drop(changed);
    assert_eq!(retained["column_0000"].row_count, 3);
    let Value::Float(value) = retained["column_2047"].mcv_values[0] else {
        panic!("float")
    };
    assert_eq!(value.to_bits(), (-0.0f64).to_bits());
    assert_eq!(
        retained["column_2047"].mcv_frequencies[0].to_bits(),
        0.5f64.to_bits()
    );
    drop(retained);
    assert_eq!(memory.used(), 0);
}

#[test]
fn statistics_capture_independently_admits_ordinary_produced_and_decoded_generations() {
    let source = fixture();
    let token = CancellationToken::new();
    let owner = MemoryBudget::new(16 << 20);
    let produced = source
        .with_replacements(
            &ColumnStatistics::default(),
            ProductionControl::new(&owner, &token, &token),
        )
        .unwrap();
    let mut reader = produced.image_reader(&owner, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    drop(reader);
    let decoded = ColumnStatistics::read_image(&mut bytes.as_slice(), &owner, &token).unwrap();
    for source in [source, produced, decoded] {
        let independent = MemoryBudget::new(16 << 20);
        let retained = source
            .capture_with_control(&ProductionControl::new(&independent, &token, &token))
            .unwrap();
        let bytes = independent.used();
        assert!(bytes > 2048 * 128);
        let denied = MemoryBudget::new(bytes - 1);
        assert_eq!(
            source
                .capture_with_control(&ProductionControl::new(&denied, &token, &token))
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
        let exact = MemoryBudget::new(bytes);
        let exact_source = source
            .capture_with_control(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), bytes);
        drop(source);
        assert_eq!(retained["column_2047"].equality_selectivity(), 0.5);
        drop((retained, exact_source));
        assert_eq!(independent.used(), 0);
        assert_eq!(exact.used(), 0);
    }
    assert_eq!(owner.used(), 0);
}

#[test]
fn statistics_capture_footprints_follow_replacement_capacity_and_both_controls() {
    let mut source = ColumnStatistics::default();
    for name in ["a", "b", "a"] {
        source.insert(
            name.into(),
            ColumnStats {
                mcv_values: vec![Value::Str("x".repeat(name.len() * 100))],
                ..Default::default()
            },
        );
        let Columns::Ordinary(columns) = &source.0 else {
            panic!("ordinary")
        };
        assert_eq!(
            *source.1.as_ref().unwrap(),
            footprint::ordinary(columns, &ProductionControl::uncontrolled()).unwrap()
        );
    }
    let retained = source.clone();
    source.insert("a".into(), ColumnStats::default());
    assert_eq!(retained["a"].mcv_values.len(), 1);
    assert!(source["a"].mcv_values.is_empty());
    let Columns::Ordinary(columns) = &source.0 else {
        panic!("ordinary")
    };
    assert_eq!(
        *source.1.as_ref().unwrap(),
        footprint::ordinary(columns, &ProductionControl::uncontrolled()).unwrap()
    );
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = MemoryBudget::new(16 << 20);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            source
                .capture_with_control(&ProductionControl::new(&memory, &original, &invoking))
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
