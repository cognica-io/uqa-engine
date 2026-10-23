//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::physical::demand::ONE;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn counting_scan(calls: &Arc<AtomicUsize>, rows: i64) -> Box<dyn PhysicalOperator> {
    let calls = Arc::clone(calls);
    Box::new(crate::PhysicalRowIteratorScan::new(
        RowSchema::new(vec!["n".into()]),
        Box::new((1..=rows).map(move |n| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(crate::PhysicalRow::from_values(vec![Value::Int(n)]))
        })),
    ))
}

fn late_error() -> ScalarExpr {
    bin(
        BinaryOp::Divide,
        ScalarExpr::Literal(Value::Int(1)),
        bin(
            BinaryOp::Subtract,
            ScalarExpr::Literal(Value::Int(2)),
            col("n"),
        ),
    )
}

#[test]
fn bounded_filter_and_projection_delay_later_expression_errors() {
    for filter in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = counting_scan(&calls, 2);
        let mut operator: Box<dyn PhysicalOperator> = if filter {
            Box::new(Filter::new(
                input,
                bin(
                    BinaryOp::Greater,
                    late_error(),
                    ScalarExpr::Literal(Value::Int(0)),
                ),
                vec![],
            ))
        } else {
            Box::new(Project::new(
                input,
                vec![("n".into(), late_error())],
                vec![],
            ))
        };
        operator.open().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let first = operator.next_with_demand(ONE).unwrap().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first.schema.view(&first.rows[0]).get("n"),
            Some(&Value::Int(1))
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(operator
            .next_with_demand(ONE)
            .unwrap_err()
            .to_string()
            .contains("division by zero"));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        operator.close().unwrap();
    }
}

#[test]
fn bounded_filter_counts_rejected_inputs_and_limit_offsets_without_overreading() {
    let calls = Arc::new(AtomicUsize::new(0));
    let filter = Filter::new(
        counting_scan(&calls, 20),
        bin(
            BinaryOp::Greater,
            col("n"),
            ScalarExpr::Literal(Value::Int(2)),
        ),
        vec![],
    );
    let projection = Project::new(
        Box::new(filter),
        vec![(
            "n".into(),
            bin(BinaryOp::Add, col("n"), ScalarExpr::Literal(Value::Int(10))),
        )],
        vec![],
    );
    let mut limit = Limit::new(Box::new(projection), 2, Some(1));
    let (_, rows) = run_to_rows(&mut limit).unwrap();
    assert_eq!(rows, vec![row([("n", Value::Int(15))])]);
    assert_eq!(calls.load(Ordering::Relaxed), 5);
}

#[test]
fn unrestricted_pipeline_retains_batches_and_zero_limit_evaluates_no_rows() {
    let calls = Arc::new(AtomicUsize::new(0));
    let filter = Filter::new(
        counting_scan(&calls, 16),
        ScalarExpr::Literal(Value::Bool(true)),
        vec![],
    );
    let mut projection = Project::new(
        Box::new(filter),
        vec![(
            "n".into(),
            bin(BinaryOp::Add, col("n"), ScalarExpr::Literal(Value::Int(1))),
        )],
        vec![],
    );
    projection.open().unwrap();
    assert_eq!(projection.next().unwrap().unwrap().len(), 16);
    projection.close().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 16);

    let calls = Arc::new(AtomicUsize::new(0));
    let mut limit = Limit::new(counting_scan(&calls, 16), 0, Some(0));
    let (_, rows) = run_to_rows(&mut limit).unwrap();
    assert!(rows.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}
