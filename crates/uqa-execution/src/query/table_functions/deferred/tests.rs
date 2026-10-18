//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::DeferredTableFunctionRows;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::Value;
use uqa_sql::SQLError;

#[test]
fn deferred_rows_start_their_call_at_the_first_row_request() {
    let starts = AtomicUsize::new(0);
    let mut rows = DeferredTableFunctionRows::new(|| {
        starts.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new([1, 2].into_iter().map(|value| {
            Ok(crate::PhysicalRow::from_values(vec![Value::Int(value)]))
        })))
    });
    assert_eq!(starts.load(Ordering::Relaxed), 0);
    let produced = rows.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        produced,
        [
            crate::PhysicalRow::from_values(vec![Value::Int(1)]),
            crate::PhysicalRow::from_values(vec![Value::Int(2)]),
        ]
    );
    assert!(rows.next().is_none());
    assert_eq!(starts.load(Ordering::Relaxed), 1);

    // A consumer that requests no row never starts the call.
    let unused = AtomicUsize::new(0);
    drop(DeferredTableFunctionRows::new(|| {
        unused.fetch_add(1, Ordering::Relaxed);
        Err(SQLError::Internal("unreachable".into()))
    }));
    assert_eq!(unused.load(Ordering::Relaxed), 0);
}

#[test]
fn deferred_rows_report_a_failed_start_once() {
    let mut rows = DeferredTableFunctionRows::new(|| {
        Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "step size cannot equal zero".into(),
        })
    });
    let error = rows.next().unwrap().unwrap_err();
    assert!(error.to_string().contains("step size cannot equal zero"));
    assert!(rows.next().is_none());
}
