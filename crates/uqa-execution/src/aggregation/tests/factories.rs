//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::ordering::AggregateSortKey;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Counts {
    attempts: AtomicUsize,
    observations: AtomicUsize,
    finishes: AtomicUsize,
    drops: AtomicUsize,
}

struct Factory {
    counts: Arc<Counts>,
    fail_at: usize,
}

impl SQLAggregateFactory for Factory {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        let attempt = self.counts.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == self.fail_at {
            return Err(SQLError::Routine {
                sqlstate: "58000".into(),
                message: "aggregate state creation failed".into(),
            });
        }
        Ok(Box::new(State(Arc::clone(&self.counts))))
    }
}

struct State(Arc<Counts>);

impl SQLAggregateState for State {
    fn observe(&mut self, _: &[Value]) -> Result<(), SQLError> {
        self.0.observations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.0.finishes.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Int(0))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn factory(fail_at: usize) -> (Arc<Counts>, AggregateAccumulatorTemplate) {
    let counts = Arc::new(Counts::default());
    let template = AggregateAccumulatorTemplate::registered(Arc::new(Factory {
        counts: Arc::clone(&counts),
        fail_at,
    }));
    (counts, template)
}

fn assert_failure(error: &SQLError) {
    assert_eq!(error.sqlstate(), Some("58000"));
    assert_eq!(error.to_string(), "aggregate state creation failed");
}

#[test]
fn creation_failure_precedes_observation_and_empty_group_finish() {
    let (counts, template) = factory(1);
    let error = match template.instantiate(1024) {
        Ok(_) => panic!("failed creation published an accumulator"),
        Err(error) => error,
    };
    assert_failure(&error);
    assert_eq!(counts.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(counts.observations.load(Ordering::SeqCst), 0);
    assert_eq!(counts.finishes.load(Ordering::SeqCst), 0);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 0);
}

#[test]
fn partial_group_creation_releases_prior_states_and_stops_at_failure() {
    let (counts, template) = factory(2);
    let error = match instantiate_aggregate_accumulators(
        &[template.clone(), template.clone(), template],
        1024,
    ) {
        Ok(_) => panic!("partial group creation succeeded"),
        Err(error) => error,
    };
    assert_failure(&error);
    assert_eq!(counts.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(counts.observations.load(Ordering::SeqCst), 0);
    assert_eq!(counts.finishes.load(Ordering::SeqCst), 0);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn replay_creation_errors_preserve_diagnostics_without_observing_buffered_rows() {
    for distinct in [false, true] {
        for budget in [1, 1 << 20] {
            let (counts, template) = factory(2);
            let mut accumulator = template.instantiate(budget).unwrap();
            for value in [3, 1, 2, 1] {
                let arguments = vec![Value::Int(value)];
                if distinct {
                    accumulator
                        .distinct
                        .insert(&Value::List(arguments), Vec::new(), None)
                        .unwrap();
                } else {
                    accumulator
                        .observe_registered(
                            arguments,
                            vec![AggregateSortKey::ascending(Value::Int(value))],
                            None,
                        )
                        .unwrap();
                }
            }
            assert_eq!(
                if distinct {
                    !accumulator.distinct.values.runs.is_empty()
                } else {
                    !accumulator.registered_ordered.runs.is_empty()
                },
                budget == 1,
            );
            assert_failure(&aggregate_value("host_aggregate", &accumulator, None).unwrap_err());
            assert_eq!(counts.attempts.load(Ordering::SeqCst), 2);
            assert_eq!(counts.observations.load(Ordering::SeqCst), 0);
            assert_eq!(counts.finishes.load(Ordering::SeqCst), 0);
            drop(accumulator);
            assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        }
    }
}
