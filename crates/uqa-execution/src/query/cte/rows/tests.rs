//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{PhysicalRow, PhysicalRowIteratorScan};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::Value;

fn producer(evaluations: &Arc<AtomicUsize>, fail_at: Option<usize>) -> Box<dyn PhysicalOperator> {
    let evaluations = Arc::clone(evaluations);
    Box::new(PhysicalRowIteratorScan::new(
        RowSchema::new(vec!["v".into()]),
        Box::new((0..16).map(move |position| {
            evaluations.fetch_add(1, Ordering::SeqCst);
            if fail_at == Some(position) {
                return Err(ExecError::SQL(SQLError::Routine {
                    sqlstate: "22012".into(),
                    message: "division by zero".into(),
                }));
            }
            Ok(PhysicalRow::from_values(vec![Value::Int(position as i64)]))
        })),
    ))
}
fn pull(scan: &mut dyn PhysicalOperator, count: usize) -> Vec<Value> {
    let batch = scan
        .next_with_demand(NonZeroUsize::new(count).unwrap())
        .unwrap()
        .unwrap();
    batch
        .into_owned_rows()
        .into_iter()
        .map(|row| row.view().value_at(0).unwrap().clone())
        .collect()
}
#[test]
fn materialized_readers_share_one_bounded_prefix_and_replay_without_effects() {
    for budget in [1, 65536] {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let source = CteRows::from_producer(producer(&evaluations, None), budget);
        let mut first = source.clone().into_operator();
        let mut second = source.clone().into_operator();
        first.open().unwrap();
        second.open().unwrap();
        assert_eq!(evaluations.load(Ordering::SeqCst), 0);
        assert_eq!(pull(first.as_mut(), 1), vec![Value::Int(0)]);
        assert_eq!(evaluations.load(Ordering::SeqCst), 1);
        assert_eq!(pull(second.as_mut(), 3), vec![Value::Int(0)]);
        assert_eq!(pull(second.as_mut(), 2), vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(evaluations.load(Ordering::SeqCst), 3);
        first.close().unwrap();
        first.open().unwrap();
        assert_eq!(
            pull(first.as_mut(), 3),
            vec![Value::Int(0), Value::Int(1), Value::Int(2)]
        );
        assert_eq!(evaluations.load(Ordering::SeqCst), 3);
        let CteRows::Prefix(prefix) = &source else {
            unreachable!()
        };
        assert_eq!(prefix.state.lock().rows.spilled_bytes() > 0, budget == 1);
        let tail = second.next().unwrap().unwrap();
        assert_eq!(tail.len(), 13);
        assert!(second.next().unwrap().is_none());
        assert_eq!(evaluations.load(Ordering::SeqCst), 16);
        assert_eq!(pull(first.as_mut(), 2), vec![Value::Int(3), Value::Int(4)]);
        assert_eq!(evaluations.load(Ordering::SeqCst), 16);
    }
}
#[test]
fn materialized_prefix_keeps_prior_rows_and_the_reached_error_without_reexecution() {
    for budget in [1, 65536] {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let source = CteRows::from_producer(producer(&evaluations, Some(1)), budget);
        let mut first = source.clone().into_operator();
        let mut second = source.into_operator();
        first.open().unwrap();
        second.open().unwrap();
        assert_eq!(pull(first.as_mut(), 1), vec![Value::Int(0)]);
        let error = first
            .next_with_demand(crate::physical::demand::ONE)
            .unwrap_err();
        assert!(
            matches!(error, ExecError::SQL(SQLError::Routine { ref sqlstate, .. }) if sqlstate=="22012")
        );
        assert_eq!(pull(second.as_mut(), 1), vec![Value::Int(0)]);
        let error = second
            .next_with_demand(crate::physical::demand::ONE)
            .unwrap_err();
        assert!(
            matches!(error, ExecError::SQL(SQLError::Routine { ref sqlstate, .. }) if sqlstate=="22012")
        );
        assert_eq!(evaluations.load(Ordering::SeqCst), 2);
    }
}
#[test]
fn unused_materialized_prefix_releases_its_producer_without_evaluating_input() {
    let evaluations = Arc::new(AtomicUsize::new(0));
    let source = CteRows::from_producer(producer(&evaluations, None), 1);
    let CteRows::Prefix(prefix) = &source else {
        unreachable!()
    };
    let weak = Arc::downgrade(&prefix.state);
    let reader = source.clone().into_operator();
    drop(source);
    assert!(weak.upgrade().is_some());
    drop(reader);
    assert!(weak.upgrade().is_none());
    assert_eq!(evaluations.load(Ordering::SeqCst), 0);
}
