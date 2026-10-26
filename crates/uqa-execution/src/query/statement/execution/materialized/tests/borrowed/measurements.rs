//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{bind_output_mode, collect_input, input, Fault, Projection, ReadMode};
use crate::query::measurement::QueryMeasurement;
use std::{sync::atomic::Ordering, time::Duration};

#[test]
fn measured_public_rows_keep_borrowed_scans_and_ordinary_projection_fallbacks() {
    for timing in [false, true] {
        for projection in [
            Projection::Plain,
            Projection::Computed,
            Projection::Metadata,
        ] {
            for count in [0, 32, 2048] {
                let (source, counts) = input(ReadMode::Borrowed, count, 0, projection, Fault::None);
                let measurement =
                    QueryMeasurement::new(timing, &uqa_core::memory::MemoryBudget::new(65_536));
                let result =
                    super::super::super::collect_measured::<()>(Some(&measurement), |mode| {
                        collect_input(source, bind_output_mode(None, mode, None)?)
                    })
                    .unwrap();
                assert_eq!(measurement.metrics().rows, (count / 2) as u64);
                assert_eq!(measurement.metrics().loops, 1);
                assert_eq!(result.rows.len(), count / 2);
                assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
                if matches!(projection, Projection::Plain) {
                    assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
                    assert_eq!(counts.borrowed.load(Ordering::Relaxed), count);
                } else {
                    assert_eq!(counts.borrowed.load(Ordering::Relaxed), 0);
                }
                assert!(measurement.metrics().startup <= measurement.metrics().total);
                if !timing {
                    assert_eq!(measurement.metrics().total, Duration::ZERO);
                }
            }
        }
    }
}

#[test]
fn measured_borrowed_predicate_errors_keep_partial_output_and_release_the_source() {
    for timing in [false, true] {
        let (source, counts) = input(
            ReadMode::Borrowed,
            8,
            1024,
            Projection::Plain,
            Fault::Divide,
        );
        let measurement =
            QueryMeasurement::new(timing, &uqa_core::memory::MemoryBudget::new(65_536));
        let error = super::super::super::collect_measured::<()>(Some(&measurement), |mode| {
            collect_input(source, bind_output_mode(None, mode, None)?)
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22012"));
        assert_eq!(error.to_string(), "division by zero");
        assert_eq!(measurement.metrics().rows, 3);
        assert_eq!(measurement.metrics().loops, 1);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn measured_batch_failure_counts_only_batches_delivered_before_the_error() {
    let mut source = super::super::Source::new(8);
    source.fail_after = Some(3);
    let closes = std::sync::Arc::clone(&source.closes);
    let emitted = std::sync::Arc::clone(&source.emitted);
    let measurement = QueryMeasurement::new(true, &uqa_core::memory::MemoryBudget::new(65_536));
    let error = super::super::super::collect_measured::<()>(Some(&measurement), |mode| {
        collect_input(Box::new(source), bind_output_mode(None, mode, None)?)
    })
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(measurement.metrics().rows, 3);
    assert_eq!(
        measurement.metrics().rows as usize,
        emitted.load(Ordering::Relaxed)
    );
    assert_eq!(measurement.metrics().loops, 1);
    assert_eq!(closes.load(Ordering::Relaxed), 1);
}
