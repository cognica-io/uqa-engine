//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unconsumed catalog/retrieval sources preserve laziness and typed execution errors.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
