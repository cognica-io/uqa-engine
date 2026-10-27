//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static CLOCK_READS: Cell<u64> = const { Cell::new(0) };
    static ORIGIN: Instant = Instant::now();
}

fn clock() -> Instant {
    CLOCK_READS.with(|reads| reads.set(reads.get() + 1));
    ORIGIN.with(|origin| *origin + Duration::from_millis(TICKS.get()))
}

fn advance(milliseconds: u64) {
    TICKS.set(TICKS.get() + milliseconds);
}

#[test]
fn initialization_without_a_pull_is_not_a_completed_execution() {
    let mut cycle = Cycle::default();
    cycle.initialize(Duration::from_millis(7)).unwrap();
    assert_eq!(cycle.finish(), NodeMetrics::default());
    assert_eq!(cycle.finish(), NodeMetrics::default());
}

#[test]
fn empty_pulls_count_one_loop_and_repeated_closure_is_inert() {
    let mut cycle = Cycle::default();
    cycle.initialize(Duration::from_millis(3)).unwrap();
    let step = Step::new(Some(clock));
    advance(2);
    cycle.consume(step.finish().unwrap()).unwrap();
    assert_eq!(
        cycle.finish(),
        NodeMetrics {
            loops: 1,
            rows: 0,
            startup: Duration::from_millis(5),
            total: Duration::from_millis(5),
        }
    );
    assert_eq!(cycle.finish(), NodeMetrics::default());
}

#[test]
fn source_time_excludes_downstream_work_and_keeps_first_output_time() {
    let mut cycle = Cycle::default();
    cycle.initialize(Duration::from_millis(3)).unwrap();
    let mut step = Step::new(Some(clock));
    advance(5);
    step.ready(1).unwrap();
    step.downstream(|| {
        advance(100);
        Ok(())
    })
    .unwrap();
    advance(2);
    step.ready(1).unwrap();
    step.downstream(|| {
        advance(200);
        Ok(())
    })
    .unwrap();
    advance(3);
    cycle.consume(step.finish().unwrap()).unwrap();
    assert_eq!(
        cycle.finish(),
        NodeMetrics {
            loops: 1,
            rows: 2,
            startup: Duration::from_millis(8),
            total: Duration::from_millis(13),
        }
    );
}

#[test]
fn timing_false_never_reads_the_clock_and_preserves_rows_and_loops() {
    CLOCK_READS.set(0);
    let mut totals = NodeMetrics::default();
    let mut cycle = Cycle::default();
    for rows in [1, 2, 3] {
        let mut step = Step::new(None);
        advance(10);
        step.ready(rows).unwrap();
        step.downstream(|| {
            advance(100);
            Ok(())
        })
        .unwrap();
        cycle.consume(step.finish().unwrap()).unwrap();
        accumulate(&mut totals, cycle.finish()).unwrap();
    }
    assert_eq!(CLOCK_READS.get(), 0);
    assert_eq!(
        totals,
        NodeMetrics {
            loops: 3,
            rows: 6,
            ..NodeMetrics::default()
        }
    );
}

#[test]
fn counters_reject_overflow_without_partially_changing_a_published_total() {
    let mut total = NodeMetrics {
        rows: u64::MAX,
        loops: 3,
        ..NodeMetrics::default()
    };
    let before = total;
    assert!(accumulate(
        &mut total,
        NodeMetrics {
            rows: 1,
            loops: 1,
            ..NodeMetrics::default()
        }
    )
    .is_err());
    assert_eq!(total, before);
}

#[test]
fn empty_intermediate_batches_do_not_claim_the_first_output_time() {
    let mut cycle = Cycle::default();
    cycle.initialize(Duration::from_millis(3)).unwrap();
    let step = Step::new(Some(clock));
    advance(2);
    cycle.consume(step.finish().unwrap()).unwrap();
    let mut step = Step::new(Some(clock));
    advance(5);
    step.ready(1).unwrap();
    advance(4);
    cycle.consume(step.finish().unwrap()).unwrap();
    assert_eq!(
        cycle.finish(),
        NodeMetrics {
            loops: 1,
            rows: 1,
            startup: Duration::from_millis(10),
            total: Duration::from_millis(14),
        }
    );
}

#[test]
fn backwards_clock_failure_preserves_the_sink_error_and_invalidates_the_step() {
    TICKS.set(10);
    let mut step = Step::new(Some(clock));
    let error = step
        .downstream::<()>(|| {
            TICKS.set(5);
            Err(uqa_sql::SQLError::Routine {
                sqlstate: "22012".into(),
                message: "original sink error".into(),
            }
            .into())
        })
        .unwrap_err();
    let error = crate::physical::physical_exec_error(error);
    assert_eq!(error.sqlstate(), Some("22012"));
    assert!(error.detail().unwrap().contains("measurements overflowed"));
    TICKS.set(20);
    assert!(step.finish().is_err());
}

#[test]
fn an_open_cycle_snapshot_does_not_consume_its_final_loop() {
    let mut cycle = Cycle::default();
    let mut step = Step::new(None);
    step.ready(2).unwrap();
    cycle.consume(step.finish().unwrap()).unwrap();
    assert_eq!(cycle.snapshot().rows, 2);
    assert_eq!(cycle.snapshot().loops, 1);
    assert_eq!(cycle.finish().rows, 2);
    assert_eq!(cycle.finish().loops, 0);
}
