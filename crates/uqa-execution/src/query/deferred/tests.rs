//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unconsumed catalog/retrieval sources preserve laziness and typed execution errors.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn deferred_callback_admission_precedes_boxing_and_tracks_invocation_or_close() {
    use std::sync::Arc;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    struct Capture {
        payload: [u8; 8192],
        drops: Arc<AtomicUsize>,
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for mode in 0..4 {
        let calls = AtomicUsize::new(0);
        let drops = Arc::new(AtomicUsize::new(0));
        let capture = Capture {
            payload: [7; 8192],
            drops: drops.clone(),
        };
        let pending = || {
            assert_eq!(capture.payload[0], 7);
            drop(capture);
            calls.fetch_add(1, Ordering::SeqCst);
            assert!(mode != 3, "callback unwind");
            Ok(
                Box::new(TableScan::from_physical_rows(RowSchema::default(), vec![]))
                    as Box<dyn PhysicalOperator>,
            )
        };
        let callback_bytes = size_of_val(&pending);
        assert!(callback_bytes >= 8192);
        let bytes = callback_bytes + size_of::<DeferredOperator<'_>>();
        let budget = MemoryBudget::new(bytes - usize::from(mode == 0));
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(DeferredOperator::from_bound_schema_with_control(
                Some(RowSchema::default()),
                pending,
                &control,
            ));
        });
        if mode == 0 {
            assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
            assert!(allocation.bytes_total < 4096, "{allocation:?}");
        } else {
            let mut scan = result.unwrap().unwrap();
            assert_eq!(budget.used(), bytes);
            scan.open().unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            if mode == 1 {
                assert!(scan.next().unwrap().is_none());
                assert_eq!(budget.used(), size_of::<DeferredOperator<'_>>());
            } else if mode == 3 {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan.next())).is_err()
                );
                assert_eq!(budget.used(), size_of::<DeferredOperator<'_>>());
            }
            scan.close().unwrap();
            assert_eq!(budget.used(), size_of::<DeferredOperator<'_>>());
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(mode == 1 || mode == 3)
        );
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let calls = AtomicUsize::new(0);
    for token in [&original, &invoking] {
        token.cancel();
        let result = DeferredOperator::from_bound_schema_with_control(
            None,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(TableScan::from_physical_rows(
                    RowSchema::default(),
                    vec![],
                )))
            },
            &control,
        );
        assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
        token.reset();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_column_binding_preserves_deferred_closed_descriptors() {
    for open in [false, true] {
        let calls = AtomicUsize::new(0);
        let runtime = RowSchema::with_types(
            vec!["value".into()],
            vec![Some(uqa_sql::ColumnType::Integer)],
        );
        let bound = if open {
            RowSchema::with_open_columns(&RowSchema::default(), None)
        } else {
            runtime.clone()
        };
        let mut scan = DeferredOperator::from_bound_schema(
            Some(bound),
            Box::new(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(TableScan::from_physical_rows(
                    runtime,
                    vec![crate::PhysicalRow::from_values(vec![uqa_core::Value::Int(
                        7,
                    )])],
                )))
            }),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(open));
        assert_eq!(scan.schema(), &["value".to_string()]);
        let (_, rows) = crate::physical::run_to_rows(scan.as_mut()).unwrap();
        assert_eq!(rows[0]["value"], uqa_core::Value::Int(7));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn physical_output_binding_keeps_scores_outside_public_columns() {
    let column = uqa_sql::ast::InternalRelationId::allocate().column(0);
    let schema = RowSchema::new(vec!["id".into()]);
    let schema = RowSchema::append_internal_typed(
        &schema,
        &[(column, Some(uqa_sql::ColumnType::DoublePrecision))],
    );
    let schema = RowSchema::with_score_source(&schema, Some("hit"), column);
    let calls = AtomicUsize::new(0);
    let mut output = DeferredOperator::from_bound_schema(
        None,
        Box::new(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TableScan::from_physical_rows(
                schema.clone(),
                vec![],
            )))
        }),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(output.row_schema(), &schema);
    assert_eq!(
        output.row_schema().score_sources().collect::<Vec<_>>(),
        [(Some("hit"), column)]
    );
    output.open().unwrap();
    assert!(output.next().unwrap().is_none());
    output.close().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn deferred_sources_run_once_on_pull_and_remain_unused_when_closed_without_rows() {
    for pull in [false, true] {
        let calls = AtomicUsize::new(0);
        let mut scan = DeferredOperator::from_rows(
            RowSchema::default(),
            Box::new(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(crate::scan::VecSource::with_row_schema(
                    RowSchema::default(),
                    vec![],
                )))
            }),
        );
        scan.open().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if pull {
            assert!(scan.next().unwrap().is_none());
            assert!(scan.next().unwrap().is_none());
        }
        scan.close().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(pull));
    }
}

#[test]
fn deferred_source_failures_keep_sqlstate_and_never_repeat_the_producer() {
    let calls = AtomicUsize::new(0);
    let mut scan = DeferredOperator::from_rows(
        RowSchema::default(),
        Box::new(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "retained participant completed".into(),
            })
        }),
    );
    scan.open().unwrap();
    let error = scan.next().unwrap_err();
    assert!(
        matches!(error, crate::ExecError::SQL(SQLError::Routine { ref sqlstate, .. }) if sqlstate == "40001")
    );
    assert!(scan.next().is_err());
    scan.close().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_operator_initialization_cannot_be_pulled_and_is_still_closed() {
    struct FailedOpen<'a> {
        schema: RowSchema,
        opened: &'a AtomicUsize,
        closed: &'a AtomicUsize,
    }
    impl PhysicalOperator for FailedOpen<'_> {
        fn row_schema(&self) -> &RowSchema {
            &self.schema
        }
        fn open(&mut self) -> ExecResult<()> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "initialization failed".into(),
            }
            .into())
        }
        fn next(&mut self) -> ExecResult<Option<Batch>> {
            panic!("failed input must not be pulled")
        }
        fn close(&mut self) -> ExecResult<()> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let opened = AtomicUsize::new(0);
    let closed = AtomicUsize::new(0);
    let mut operator = DeferredOperator::new(
        RowSchema::default(),
        Box::new(|| {
            Ok(Box::new(FailedOpen {
                schema: RowSchema::default(),
                opened: &opened,
                closed: &closed,
            }))
        }),
    );
    operator.open().unwrap();
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    assert!(
        matches!(operator.next(), Err(crate::ExecError::SQL(SQLError::Routine { sqlstate, .. })) if sqlstate == "40001")
    );
    assert!(operator.next().is_err());
    operator.close().unwrap();
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[test]
fn rejected_deferred_descriptor_closes_the_created_input_once() {
    struct Input<'a> {
        schema: RowSchema,
        closed: &'a AtomicUsize,
    }
    impl PhysicalOperator for Input<'_> {
        fn row_schema(&self) -> &RowSchema {
            &self.schema
        }
        fn open(&mut self) -> ExecResult<()> {
            panic!("descriptor rejected before open")
        }
        fn next(&mut self) -> ExecResult<Option<Batch>> {
            panic!("descriptor rejected before pull")
        }
        fn close(&mut self) -> ExecResult<()> {
            assert_eq!(self.closed.fetch_add(1, Ordering::SeqCst), 0);
            Err(SQLError::Routine {
                sqlstate: "58030".into(),
                message: "deferred input close failed".into(),
            }
            .into())
        }
    }
    let closed = AtomicUsize::new(0);
    let mut operator = DeferredOperator::new(
        RowSchema::default(),
        Box::new(|| {
            Ok(Box::new(Input {
                schema: RowSchema::new(vec!["unexpected".into()]),
                closed: &closed,
            }))
        }),
    );
    operator.open().unwrap();
    let error = crate::physical::physical_exec_error(operator.next().unwrap_err());
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error.to_string().contains("changed its bound row schema"));
    assert!(error
        .detail()
        .unwrap()
        .contains("deferred input close failed"));
    operator.close().unwrap();
    operator.close().unwrap();
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}
