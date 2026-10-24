//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{PhysicalRow, PhysicalRowIteratorScan, RowSchema, SQLRowSource, SQLRowStream};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
struct Counts {
    close: AtomicUsize,
    drop: AtomicUsize,
}

struct Source {
    counts: Arc<Counts>,
    label: &'static str,
}

impl SQLRowSource<PhysicalRow> for Source {
    fn next_row(&mut self) -> Result<Option<PhysicalRow>, SQLError> {
        panic!("construction must not pull input rows")
    }
    fn close(&mut self) -> Result<(), SQLError> {
        assert_eq!(self.counts.close.fetch_add(1, Ordering::SeqCst), 0);
        Err(SQLError::Routine {
            sqlstate: "58030".into(),
            message: self.label.into(),
        })
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        assert_eq!(self.counts.close.load(Ordering::SeqCst), 1);
        self.counts.drop.fetch_add(1, Ordering::SeqCst);
    }
}

fn input(width: usize, label: &'static str) -> (Input<'static>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let schema = RowSchema::new((0..width).map(|n| format!("n{n}")).collect());
    let input = Box::new(PhysicalRowIteratorScan::from_stream(
        schema,
        SQLRowStream::from_source(Source {
            counts: counts.clone(),
            label,
        }),
    ));
    (input, counts)
}

fn primary() -> SQLError {
    SQLError::Routine {
        sqlstate: "22012".into(),
        message: "construction failed".into(),
    }
}

fn closed(counts: &Counts) {
    assert_eq!(counts.close.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_parent_construction_closes_wrapped_inputs_and_keeps_both_diagnostics() {
    let (input, counts) = input(1, "scan close failed");
    let result = OperatorBuild::build(input, |input| {
        input.wrap(|input| {
            Box::new(crate::ColumnSelection::with_positions(
                input,
                vec![("output".into(), 0)],
            ))
        });
        Err(primary())
    });
    let Err(error) = result else {
        panic!("construction unexpectedly succeeded")
    };
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(error.to_string(), "construction failed");
    assert!(error.detail().unwrap().contains("scan close failed"));
    closed(&counts);
}

#[test]
fn nested_construction_transfers_cleanup_and_releases_both_failed_inputs() {
    let (left, left_counts) = input(1, "left close failed");
    let (right, right_counts) = input(1, "right close failed");
    let result = OperatorBuild::pair(left, right, |left, _| {
        left.attach(|input| OperatorBuild::build(input, |_| Err(primary())))
    });
    let Err(error) = result else {
        panic!("construction unexpectedly succeeded")
    };
    assert_eq!(error.sqlstate(), Some("22012"));
    let detail = error.detail().unwrap();
    assert_eq!(detail.matches("left close failed").count(), 1);
    assert_eq!(detail.matches("right close failed").count(), 1);
    closed(&left_counts);
    closed(&right_counts);
}

#[test]
fn successful_construction_transfers_the_live_scan_without_closing_it() {
    let (input, counts) = input(1, "later close failed");
    let mut built = OperatorBuild::build(input, |_| Ok(())).unwrap();
    assert_eq!(counts.close.load(Ordering::SeqCst), 0);
    assert_eq!(
        crate::physical::physical_exec_error(built.close().unwrap_err()).sqlstate(),
        Some("58030")
    );
    drop(built);
    closed(&counts);
}

#[test]
fn rejected_set_width_closes_both_inputs_in_the_native_constructor() {
    for (right_width, explicit_types) in [(2, None), (2, Some(vec![None])), (1, Some(vec![]))] {
        let (left, left_counts) = input(1, "left close failed");
        let (right, right_counts) = input(right_width, "right close failed");
        let result = match explicit_types {
            None => crate::ExternalSetOperation::new(
                left,
                right,
                uqa_sql::ast::SetOpKind::Union,
                true,
                1024,
            ),
            Some(types) => crate::ExternalSetOperation::new_with_types(
                left,
                right,
                uqa_sql::ast::SetOpKind::Union,
                true,
                types,
                1024,
            ),
        };
        let Err(error) = result else {
            panic!("different widths were accepted")
        };
        let error = crate::physical::physical_exec_error(error);
        assert!(error.to_string().contains("width"));
        let detail = error.detail().unwrap();
        assert!(detail.contains("left close failed"));
        assert!(detail.contains("right close failed"));
        closed(&left_counts);
        closed(&right_counts);
    }
}

#[test]
fn rejected_native_join_alias_releases_an_already_created_input() {
    let (input, counts) = input(1, "alias input close failed");
    let result = crate::query::source_projection::alias_join_operator(
        input,
        Some("joined"),
        &["one".into(), "two".into()],
    );
    let Err(error) = result else {
        panic!("invalid aliases were accepted")
    };
    assert_eq!(error.sqlstate(), Some("42P10"));
    assert!(error.detail().unwrap().contains("alias input close failed"));
    closed(&counts);
}
