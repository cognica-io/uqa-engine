//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete borrowed results preserve first errors, cancellation and operator cleanup.

use super::{input, Fault, Projection, ReadMode};
use crate::{
    query::output::MaterializedResultSink, Batch, ExecError, ExecResult, PhysicalOperator,
    RowSchema,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Closing {
    input: Box<dyn PhysicalOperator>,
    closes: Arc<AtomicUsize>,
    fail: bool,
}

impl PhysicalOperator for Closing {
    fn row_schema(&self) -> &RowSchema {
        self.input.row_schema()
    }
    fn supports_batched_consumption(&self) -> bool {
        self.input.supports_batched_consumption()
    }
    fn open(&mut self) -> ExecResult<()> {
        self.input.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.input.next()
    }
    fn collect_materialized(&mut self, sink: &mut MaterializedResultSink<'_>) -> ExecResult<bool> {
        self.input.collect_materialized(sink)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        self.input.close()?;
        if self.fail {
            Err(ExecError::Other("cleanup marker".into()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn borrowed_public_results_keep_predicate_errors_and_close_after_partial_output() {
    for read in [ReadMode::Shared, ReadMode::Borrowed] {
        for cleanup_failure in [false, true] {
            let (source, counts) = input(read, 8, 1024, Projection::Plain, Fault::Divide);
            let closes = Arc::new(AtomicUsize::new(0));
            let source = Box::new(Closing {
                input: source,
                closes: Arc::clone(&closes),
                fail: cleanup_failure,
            });
            let error = super::super::super::collect::<()>(|mode| {
                super::collect_input(source, super::bind_output_mode(None, mode, None)?)
            })
            .unwrap_err();
            assert!(error.to_string().contains("division by zero"), "{error}");
            assert_eq!(
                error.to_string().contains("cleanup marker"),
                cleanup_failure,
                "{error}"
            );
            assert_eq!(closes.load(Ordering::Relaxed), 1);
            assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
            if matches!(read, ReadMode::Borrowed) {
                assert_eq!(counts.borrowed.load(Ordering::Relaxed), 4);
                assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
            }
        }
    }
}

#[test]
fn borrowed_public_results_observe_cancellation_between_rejected_input_pages() {
    for before_start in [false, true] {
        let token = uqa_core::CancellationToken::new();
        if before_start {
            token.cancel();
        }
        let (source, counts) = input(
            ReadMode::Borrowed,
            8,
            1024,
            Projection::Plain,
            Fault::CancelAfter(1, token.clone()),
        );
        let closes = Arc::new(AtomicUsize::new(0));
        let source = Box::new(Closing {
            input: source,
            closes: Arc::clone(&closes),
            fail: false,
        });
        let error = super::super::super::collect::<()>(|mode| {
            super::super::collect_input_with_cancellation(
                source,
                super::bind_output_mode(None, mode, None)?,
                &token,
            )
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"), "{error}");
        assert_eq!(
            counts.borrowed.load(Ordering::Relaxed),
            usize::from(!before_start)
        );
        assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
        assert_eq!(closes.load(Ordering::Relaxed), 1);
        assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
    }
}
