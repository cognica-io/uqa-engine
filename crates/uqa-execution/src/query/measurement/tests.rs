//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

thread_local! {
    static CLOCK: Cell<(Instant, Duration, u64)> = Cell::new((Instant::now(), Duration::ZERO, 0));
}

fn now() -> Instant {
    CLOCK.with(|clock| {
        let (base, elapsed, calls) = clock.get();
        clock.set((base, elapsed, calls + 1));
        base + elapsed
    })
}

fn advance(millis: u64) {
    CLOCK.with(|clock| {
        let (base, elapsed, calls) = clock.get();
        clock.set((base, elapsed + Duration::from_millis(millis), calls));
    });
}

fn measurement(timing: bool) -> QueryMeasurement {
    CLOCK.with(|clock| clock.set((Instant::now(), Duration::ZERO, 0)));
    let mut measurement = QueryMeasurement::new(timing);
    if timing {
        measurement.clock = Some(now);
    }
    measurement
}

#[test]
fn first_produced_row_excludes_all_completed_destination_work() {
    let measurement = measurement(true);
    measurement
        .run(|| {
            advance(10);
            measurement.output(1, || {
                advance(100);
                Ok(())
            })?;
            advance(20);
            measurement.output(2, || {
                advance(50);
                Ok(())
            })?;
            advance(5);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        measurement.metrics(),
        ExplainNodeMetrics {
            rows: 3,
            loops: 1,
            startup: Duration::from_millis(10),
            total: Duration::from_millis(35),
        }
    );
}

#[test]
fn empty_delivery_does_not_establish_startup_and_never_run_has_no_loop() {
    let measurement = measurement(true);
    assert_eq!(measurement.metrics(), ExplainNodeMetrics::default());
    measurement
        .run(|| {
            advance(5);
            measurement.output(0, || {
                advance(7);
                Ok(())
            })?;
            advance(11);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        measurement.metrics(),
        ExplainNodeMetrics {
            rows: 0,
            loops: 1,
            startup: Duration::from_millis(16),
            total: Duration::from_millis(16),
        }
    );
}

#[test]
fn initial_empty_batches_leave_startup_for_the_first_nonempty_batch() {
    let measurement = measurement(true);
    measurement
        .run(|| {
            advance(5);
            measurement.output(0, || {
                advance(7);
                Ok(())
            })?;
            advance(11);
            measurement.output(2, || {
                advance(17);
                Ok(())
            })?;
            advance(3);
            Ok(())
        })
        .unwrap();
    assert_eq!(measurement.metrics().startup, Duration::from_millis(16));
    assert_eq!(measurement.metrics().total, Duration::from_millis(19));
}

#[test]
fn untimed_invocations_count_loops_and_rows_without_clock_reads_or_allocations() {
    let measurement = measurement(false);
    let allocations = allocation_counter::measure(|| {
        for _ in 0..1000 {
            measurement
                .run(|| measurement.output(7, || Ok(())))
                .unwrap();
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(
        measurement.metrics(),
        ExplainNodeMetrics {
            rows: 7000,
            loops: 1000,
            ..ExplainNodeMetrics::default()
        }
    );
    assert_eq!(CLOCK.with(|clock| clock.get().2), 0);
}

#[test]
fn completed_invocations_accumulate_without_reusing_startup() {
    let measurement = measurement(true);
    for delay in [5, 13] {
        measurement
            .run(|| {
                advance(delay);
                measurement.output(1, || Ok(()))?;
                advance(2);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        measurement.metrics(),
        ExplainNodeMetrics {
            rows: 2,
            loops: 2,
            startup: Duration::from_millis(18),
            total: Duration::from_millis(22),
        }
    );
}

#[test]
fn row_overflow_does_not_replace_the_last_count_or_invoke_the_sink() {
    let measurement = measurement(false);
    measurement
        .run(|| {
            let mut running = measurement.running.get().unwrap();
            running.rows = u64::MAX;
            measurement.running.set(Some(running));
            assert!(measurement
                .output::<()>(1, || panic!("overflow reached destination"))
                .is_err());
            assert_eq!(measurement.running.get().unwrap().rows, u64::MAX);
            Ok(())
        })
        .unwrap();
    assert_eq!(measurement.metrics().rows, u64::MAX);
}

#[test]
fn original_execution_error_survives_completion_overflow() {
    let measurement = measurement(false);
    let original = ExplainNodeMetrics {
        loops: u64::MAX,
        ..ExplainNodeMetrics::default()
    };
    measurement.completed.set(original);
    let error = measurement
        .run::<()>(|| {
            Err(SQLError::Routine {
                sqlstate: "22012".into(),
                message: "source failure".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert!(error.to_string().contains("source failure"));
    assert_eq!(measurement.metrics(), original);
    assert!(measurement.running.get().is_none());
}

#[test]
fn rejected_reentry_and_unwinding_restore_the_invocation_owner() {
    let measurement = measurement(false);
    measurement
        .run(|| {
            assert!(measurement.run(|| Ok(())).is_err());
            measurement.output(1, || Ok(()))
        })
        .unwrap();
    let original = measurement.metrics();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = measurement.run::<()>(|| panic!("source panic"));
    }));
    assert!(panic.is_err());
    assert_eq!(measurement.metrics(), original);
    assert!(measurement.running.get().is_none());
    measurement.run(|| Ok(())).unwrap();
    assert_eq!(measurement.metrics().loops, 2);
}
