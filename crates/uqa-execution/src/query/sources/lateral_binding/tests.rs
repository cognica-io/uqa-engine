//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Batch, ExecResult};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;

#[derive(Default)]
struct Counts {
    opened: AtomicUsize,
    pulled: AtomicUsize,
    closed: AtomicUsize,
}

struct Outer {
    schema: RowSchema,
    counts: Arc<Counts>,
    empty: bool,
    close_error: bool,
}

impl PhysicalOperator for Outer {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        assert_eq!(self.counts.opened.fetch_add(1, Ordering::SeqCst), 0);
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        panic!("metadata must use bounded demand")
    }
    fn next_with_demand(&mut self, demand: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        assert_eq!(demand.get(), 1);
        let index = self.counts.pulled.fetch_add(1, Ordering::SeqCst);
        Ok((!self.empty).then(|| {
            Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(vec![Value::Int(
                    i64::try_from(index).unwrap() + 1,
                )])],
            )
        }))
    }
    fn close(&mut self) -> ExecResult<()> {
        assert_eq!(self.counts.closed.fetch_add(1, Ordering::SeqCst), 0);
        if self.close_error {
            Err(SQLError::Routine {
                sqlstate: "58030".into(),
                message: "outer close failed".into(),
            }
            .into())
        } else {
            Ok(())
        }
    }
}

fn outer(empty: bool, close_error: bool) -> (Input<'static>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    (
        Box::new(Outer {
            schema: RowSchema::new(vec!["n".into()]),
            counts: counts.clone(),
            empty,
            close_error,
        }),
        counts,
    )
}

#[test]
fn descriptor_binding_replays_one_actual_outer_row_without_reopening_or_reading_ahead() {
    let (input, counts) = outer(false, false);
    let (mut left, mut right) = describe_from_first_row(input, |first| {
        assert_eq!(first.get("n"), Some(&Value::Int(1)));
        Ok(Box::new(crate::TableScan::from_physical_rows(
            RowSchema::new(vec!["value".into()]),
            vec![],
        )))
    })
    .unwrap();
    assert_eq!(counts.pulled.load(Ordering::SeqCst), 1);
    assert_eq!(right.schema(), ["value"]);
    left.open().unwrap();
    for expected in [1, 2] {
        let batch = left
            .next_with_demand(crate::physical::demand::ONE)
            .unwrap()
            .unwrap();
        assert_eq!(batch.into_result_rows()[0]["n"], Value::Int(expected));
        assert_eq!(
            counts.pulled.load(Ordering::SeqCst),
            usize::try_from(expected).unwrap()
        );
        assert_eq!(counts.opened.load(Ordering::SeqCst), 1);
    }
    right.close().unwrap();
    left.close().unwrap();
    left.close().unwrap();
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
}

#[test]
fn descriptor_failure_closes_the_open_outer_source_and_retains_cleanup_error() {
    let (input, counts) = outer(false, true);
    let error = match describe_from_first_row(input, |_| {
        Err(SQLError::Routine {
            sqlstate: "22012".into(),
            message: "descriptor invocation failed".into(),
        })
    }) {
        Err(error) => error,
        Ok(_) => panic!("descriptor must fail"),
    };
    assert_eq!(error.sqlstate(), Some("22012"));
    assert!(error.detail().unwrap().contains("outer close failed"));
    assert_eq!(counts.pulled.load(Ordering::SeqCst), 1);
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
    let (input, counts) = outer(true, false);
    let error = match describe_from_first_row(input, |_| panic!("empty input has no invocation")) {
        Err(error) => error,
        Ok(_) => panic!("no descriptor"),
    };
    assert_eq!(error.sqlstate(), Some("42P18"));
    assert_eq!(counts.closed.load(Ordering::SeqCst), 1);
}
