//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{PhysicalScanDirection, ScrollMaterialize};
use uqa_sql::{ColumnType, SQLError};

#[test]
fn range_ties_preserve_selected_types_in_forward_and_backward_scans() {
    for kind in ["numrange", "nummultirange"] {
        let ty = ColumnType::from_sql_name(kind).unwrap();
        let values = ["[1.0,3.00)", "[1.00,3.0)", "[4,5)"].map(|input| {
            let input = if kind == "nummultirange" {
                format!("{{{input}}}")
            } else {
                input.into()
            };
            uqa_sql::expr::cast_value(&Value::Str(input), kind).unwrap()
        });
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for (ty, expected) in [(ty, 2), (domain, 2), (ColumnType::Text, 1)] {
            for nulls in [false, true] {
                let values = if nulls {
                    vec![Value::Null, Value::Null, values[2].clone()]
                } else {
                    values.to_vec()
                };
                let count = if nulls { 2 } else { expected };
                for directional in [false, true] {
                    let scan = Box::new(TableScan::from_typed_rows(
                        vec!["r".into()],
                        vec![Some(ty.clone())],
                        values.iter().cloned().map(|v| row([("r", v)])).collect(),
                    ));
                    let input: Box<dyn PhysicalOperator> = if directional {
                        Box::new(ScrollMaterialize::new(scan))
                    } else {
                        scan
                    };
                    let mut limit = Limit::with_ties(
                        input,
                        0,
                        1,
                        vec![SortKey {
                            expr: col("r"),
                            descending: false,
                            nulls_first: Some(true),
                        }],
                        DefaultExpressionEvaluator::shared(vec![]),
                    );
                    if directional {
                        limit.open().unwrap();
                        let expected = values[..count].to_vec();
                        assert_eq!(drain(&mut limit, PhysicalScanDirection::Forward), expected);
                        assert_eq!(
                            drain(&mut limit, PhysicalScanDirection::Backward),
                            expected.iter().cloned().rev().collect::<Vec<_>>()
                        );
                        limit.rewind().unwrap();
                        assert_eq!(drain(&mut limit, PhysicalScanDirection::Forward), expected);
                        limit.close().unwrap();
                    } else {
                        let (_, rows) = run_to_rows(&mut limit).unwrap();
                        assert_eq!(rows.len(), count, "{kind}: {ty:?}");
                        for (row, input) in rows.iter().zip(&values) {
                            assert!(row["r"].has_same_representation(input));
                        }
                    }
                }
            }
        }
    }
}

fn drain(limit: &mut Limit<'_>, direction: PhysicalScanDirection) -> Vec<Value> {
    let mut output = Vec::new();
    while let Some(batch) = limit.next_direction(direction).unwrap() {
        output.extend(batch.rows.iter().map(|row| row.value(0).unwrap().clone()));
    }
    output
}

#[test]
fn with_ties_close_releases_boundary_and_types_even_when_child_close_fails() {
    use std::sync::{atomic::AtomicUsize, Arc};

    const PAYLOAD_BYTES: usize = 64 * 1024;
    let ty = ColumnType::Domain {
        schema: "public".into(),
        name: "retained_text_domain".into(),
        oid: 50_001,
        array_oid: Some(50_002),
        collation_oid: None,
        base: Box::new(ColumnType::Text),
    };
    for (populated, directional) in [(false, false), (true, false), (true, true)] {
        for fail_close in [false, true] {
            let rows = if directional {
                vec![row([("r", Value::Str("a".repeat(PAYLOAD_BYTES)))])]
            } else if populated {
                vec![
                    row([("r", Value::Str("a".repeat(PAYLOAD_BYTES)))]),
                    row([("r", Value::Str("a".repeat(PAYLOAD_BYTES)))]),
                    row([("r", Value::Str("z".into()))]),
                ]
            } else {
                vec![]
            };
            let calls = Arc::new(AtomicUsize::new(0));
            let scan = ClosingScan {
                child: TableScan::from_typed_rows(vec!["r".into()], vec![Some(ty.clone())], rows),
                fail_close,
            };
            let evaluator = CountingTypes {
                inner: DefaultExpressionEvaluator::shared(vec![]),
                calls: Arc::clone(&calls),
            };
            let mut limit = Limit::with_ties(
                Box::new(scan),
                0,
                1,
                vec![SortKey {
                    expr: col("r"),
                    descending: false,
                    nulls_first: None,
                }],
                Arc::new(evaluator),
            );
            for opened in 1..=2 {
                limit.open().unwrap();
                let has_boundary = populated && opened == 1;
                let next = |limit: &mut Limit<'_>| {
                    if directional {
                        limit.next_direction(PhysicalScanDirection::Forward)
                    } else {
                        limit.next()
                    }
                };
                let mut returned = 0;
                while let Some(batch) = next(&mut limit).unwrap() {
                    returned += batch.len();
                }
                assert_eq!(
                    returned,
                    if has_boundary {
                        if directional {
                            1
                        } else {
                            2
                        }
                    } else {
                        0
                    }
                );
                assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), opened);
                let allocations = allocation_counter::measure(|| {
                    assert_eq!(limit.close().is_err(), fail_close);
                });
                let retained = "public".len()
                    + "retained_text_domain".len()
                    + usize::from(has_boundary) * (1 + usize::from(directional)) * PAYLOAD_BYTES;
                assert!(
                    allocations.bytes_current <= -(retained as i64),
                    "populated={populated}, directional={directional}, fail_close={fail_close}, open={opened}: {allocations:?}"
                );
                assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), opened);
            }
        }
    }
}

struct ClosingScan {
    child: TableScan,
    fail_close: bool,
}

impl PhysicalOperator for ClosingScan {
    fn row_schema(&self) -> &RowSchema {
        self.child.row_schema()
    }
    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.child.next()
    }
    fn close(&mut self) -> ExecResult<()> {
        // Retain the child fixture so this check isolates the LIMIT owner's release.
        if self.fail_close {
            Err(ExecError::Other("child close failed".into()))
        } else {
            Ok(())
        }
    }
}

struct CountingTypes {
    inner: SharedExpressionEvaluator<'static>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ExpressionEvaluator for CountingTypes {
    fn evaluate(&self, expr: &ScalarExpr, row: &dyn uqa_sql::expr::RowLookup) -> ExecResult<Value> {
        self.inner.evaluate(expr, row)
    }
    fn expression_type(
        &self,
        expr: &ScalarExpr,
        schema: &RowSchema,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.expression_type(expr, schema)
    }
}
