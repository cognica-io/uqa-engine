//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Empty probes preserve PostgreSQL's demand for independent function inputs.

use super::*;

#[test]
fn empty_left_inputs_request_right_rows_only_when_the_join_preserves_them() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
    ))
    .unwrap();
    for (kind, name) in [
        (JoinKind::Cross, "cross"),
        (JoinKind::Inner, "inner"),
        (JoinKind::Left, "left"),
        (JoinKind::Right, "right"),
        (JoinKind::Full, "full"),
    ] {
        let id = format!("empty_volatile_{name}");
        let reference = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap();
        let expected_rows = reference["results"][0]["rows"].as_array().unwrap().len();
        for hash in [false, true] {
            if hash && kind == JoinKind::Cross {
                continue;
            }
            for bytes in [1, 64 * 1024] {
                let requested = Arc::new(AtomicUsize::new(0));
                let calls = Arc::clone(&requested);
                let right = crate::PhysicalRowIteratorScan::new(
                    RowSchema::with_types(
                        vec!["r.k".into()],
                        vec![Some(uqa_sql::ColumnType::Integer)],
                    ),
                    Box::new((1..=2).map(move |value| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(PhysicalRow::from_values(vec![Value::Int(value)]))
                    })),
                );
                let left = TableScan::from_rows(vec!["l.k".into()], vec![]);
                let mut join: Box<dyn PhysicalOperator> = if hash {
                    let mut join = HashJoin::new_with_work_mem(
                        Box::new(left),
                        Box::new(right),
                        kind,
                        vec![ScalarExpr::Column("l.k".into())],
                        vec![ScalarExpr::Column("r.k".into())],
                        evaluator(),
                        ResultRow::new(),
                        ResultRow::new(),
                        bytes,
                    );
                    // Exercise an empty probe even when a different cardinality estimate could choose it as the build input.
                    join.build_left = false;
                    Box::new(join)
                } else {
                    Box::new(NestedLoopJoin::new_with_work_mem(
                        Box::new(left),
                        Box::new(right),
                        kind,
                        None,
                        evaluator(),
                        ResultRow::new(),
                        ResultRow::new(),
                        bytes,
                    ))
                };
                let (_, rows) = run_to_rows(join.as_mut()).unwrap();
                assert_eq!(
                    rows.len(),
                    expected_rows,
                    "{id}, hash={hash}, bytes={bytes}"
                );
                assert_eq!(
                    requested.load(Ordering::Relaxed),
                    expected_rows,
                    "{id}, hash={hash}, bytes={bytes}"
                );
            }
        }
    }
}
