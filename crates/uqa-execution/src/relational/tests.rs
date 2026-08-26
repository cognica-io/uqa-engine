//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::*;
use crate::physical::run_to_rows;
use crate::scan::{RowSource, TableScan};
use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, InternalRelationId};

fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn boxed_scan(schema: Vec<String>, rows: Vec<ResultRow>) -> Box<dyn PhysicalOperator> {
    Box::new(TableScan::from_rows(schema, rows))
}

fn col(name: &str) -> ScalarExpr {
    ScalarExpr::Column(name.into())
}

fn bin(op: BinaryOp, lhs: ScalarExpr, rhs: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

struct OrderedSource {
    rows: std::vec::IntoIter<ResultRow>,
    schema: Vec<String>,
    ordering: Vec<crate::PhysicalOrder>,
}

impl RowSource for OrderedSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        &self.ordering
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        Ok(self.rows.next())
    }
}

fn ascending_id_scan() -> Box<dyn PhysicalOperator> {
    Box::new(TableScan::new(Box::new(OrderedSource {
        rows: vec![row([("id", Value::Int(1))]), row([("id", Value::Int(2))])].into_iter(),
        schema: vec!["id".into()],
        ordering: vec![crate::PhysicalOrder {
            position: 0,
            descending: false,
            nulls_first: None,
            nullable: false,
        }],
    })))
}

#[test]
fn appending_projection_only_propagates_unmodified_ordering_prefix() {
    let preserved = Project::appending(
        ascending_id_scan(),
        vec![("derived".into(), ScalarExpr::Literal(Value::Int(1)))],
        vec![],
    );
    assert_eq!(preserved.output_ordering()[0].position, 0);

    let overwritten = Project::appending(
        ascending_id_scan(),
        vec![("id".into(), ScalarExpr::Literal(Value::Int(0)))],
        vec![],
    );
    assert!(overwritten.output_ordering().is_empty());
}

#[test]
fn direct_projection_is_a_schema_only_row_remap() {
    let scan = TableScan::from_physical_rows(
        RowSchema::new(vec!["first".into(), "second".into()]),
        vec![crate::PhysicalRow::from_values(vec![
            Value::Str("payload".repeat(32)),
            Value::Int(7),
        ])],
    );
    let mut projection = Project::new(
        Box::new(scan),
        vec![("value".into(), col("second"))],
        vec![],
    );

    projection.open().unwrap();
    let batch = projection.next().unwrap().unwrap();
    projection.close().unwrap();

    assert_eq!(batch.schema.physical_width(), 2);
    assert_eq!(batch.rows[0].fragment_count(), 1);
    assert_eq!(
        batch.schema.view(&batch.rows[0]).get("value"),
        Some(&Value::Int(7))
    );
}

#[test]
fn mixed_projection_appends_only_computed_values() {
    let scan = TableScan::from_physical_rows(
        RowSchema::new(vec!["first".into(), "second".into()]),
        vec![crate::PhysicalRow::from_values(vec![
            Value::Int(3),
            Value::Int(7),
        ])],
    );
    let mut projection = Project::new(
        Box::new(scan),
        vec![
            ("second".into(), col("second")),
            (
                "sum".into(),
                bin(BinaryOp::Add, col("first"), col("second")),
            ),
        ],
        vec![],
    );

    projection.open().unwrap();
    let batch = projection.next().unwrap().unwrap();
    projection.close().unwrap();

    assert_eq!(batch.schema.physical_width(), 3);
    assert_eq!(batch.rows[0].fragment_count(), 2);
    let view = batch.schema.view(&batch.rows[0]);
    assert_eq!(view.get("second"), Some(&Value::Int(7)));
    assert_eq!(view.get("sum"), Some(&Value::Int(10)));
}

#[test]
fn internal_projection_targets_never_enter_the_sql_namespace() {
    let relation = InternalRelationId::allocate();
    let internal = relation.column(0);
    let scan = TableScan::from_physical_rows(
        RowSchema::new(vec!["value".into()]),
        vec![crate::PhysicalRow::from_values(vec![Value::Int(7)])],
    );
    let mut projection = Project::appending_targets(
        Box::new(scan),
        vec![(
            ProjectionTarget::Internal(internal),
            bin(
                BinaryOp::Add,
                col("value"),
                ScalarExpr::Literal(Value::Int(1)),
            ),
        )],
        vec![],
    );

    projection.open().unwrap();
    let batch = projection.next().unwrap().unwrap();
    projection.close().unwrap();

    assert_eq!(batch.schema.columns(), ["value"]);
    assert_eq!(batch.schema.physical_width(), 2);
    let slot = batch.schema.internal_slot(internal).unwrap();
    assert_eq!(batch.rows[0].value(slot), Some(&Value::Int(8)));
}

#[test]
fn filter_keeps_truthy_rows() {
    let scan = boxed_scan(
        vec!["x".into()],
        vec![
            row([("x", Value::Int(1))]),
            row([("x", Value::Int(2))]),
            row([("x", Value::Int(3))]),
        ],
    );
    let predicate = bin(
        BinaryOp::Greater,
        col("x"),
        ScalarExpr::Literal(Value::Int(1)),
    );
    let mut filt = Filter::new(scan, predicate, vec![]);
    let (_cols, rows) = run_to_rows(&mut filt).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn filter_propagates_expression_errors() {
    let scan = boxed_scan(vec!["x".into()], vec![row([("x", Value::Int(1))])]);
    let zero = bin(BinaryOp::Subtract, col("x"), col("x"));
    let division = bin(BinaryOp::Divide, col("x"), zero);
    let predicate = bin(
        BinaryOp::Greater,
        division,
        ScalarExpr::Literal(Value::Int(0)),
    );
    let mut filter = Filter::new(scan, predicate, vec![]);
    let error = run_to_rows(&mut filter).unwrap_err();
    assert!(error.to_string().contains("division by zero"));
}

#[test]
fn limit_with_offset() {
    let scan = boxed_scan(
        vec!["x".into()],
        (0..10)
            .map(|i| row([("x", Value::Int(i as i64))]))
            .collect(),
    );
    let mut lim = Limit::new(scan, 3, Some(4));
    let (_cols, rows) = run_to_rows(&mut lim).unwrap();
    assert_eq!(rows.len(), 4);
    let xs: Vec<i64> = rows
        .iter()
        .map(|r| match r["x"] {
            Value::Int(i) => i,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(xs, vec![3, 4, 5, 6]);
}

#[test]
fn fetch_with_ties_extends_the_boundary_across_batches() {
    let mut rows = (0..1022)
        .map(|id| row([("id", Value::Int(id)), ("key", Value::Int(id))]))
        .collect::<Vec<_>>();
    rows.extend([
        row([("id", Value::Int(1022)), ("key", Value::Int(10_000))]),
        row([("id", Value::Int(1023)), ("key", Value::Int(20_000))]),
        row([("id", Value::Int(1024)), ("key", Value::Int(20_000))]),
        row([("id", Value::Int(1025)), ("key", Value::Int(20_000))]),
        row([("id", Value::Int(1026)), ("key", Value::Int(30_000))]),
    ]);
    let scan = boxed_scan(vec!["id".into(), "key".into()], rows);
    let mut limit = Limit::with_ties(
        scan,
        1022,
        2,
        vec![SortKey {
            expr: col("key"),
            descending: false,
            nulls_first: None,
        }],
        DefaultExpressionEvaluator::shared(vec![]),
    );
    let (_, rows) = run_to_rows(&mut limit).unwrap();
    let ids = rows
        .iter()
        .map(|row| match row["id"] {
            Value::Int(id) => id,
            ref other => panic!("unexpected id: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, [1022, 1023, 1024, 1025]);
}

#[test]
fn sort_descending() {
    let scan = boxed_scan(
        vec!["x".into()],
        vec![
            row([("x", Value::Int(2))]),
            row([("x", Value::Int(1))]),
            row([("x", Value::Int(3))]),
        ],
    );
    let mut sort = Sort::new(
        scan,
        vec![SortKey {
            expr: col("x"),
            descending: true,
            nulls_first: None,
        }],
        vec![],
    );
    let (_cols, rows) = run_to_rows(&mut sort).unwrap();
    let xs: Vec<i64> = rows
        .iter()
        .map(|r| match r["x"] {
            Value::Int(i) => i,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(xs, vec![3, 2, 1]);
}

#[test]
fn physical_sort_comparison_preserves_numeric_total_order() {
    assert_eq!(
        compare_values(
            &Value::Int(9_007_199_254_740_993),
            &Value::Float(9_007_199_254_740_992.0),
        ),
        std::cmp::Ordering::Greater
    );
    assert_eq!(
        compare_values(&Value::Float(f64::NAN), &Value::Float(f64::INFINITY)),
        std::cmp::Ordering::Greater
    );
    assert_ne!(
        compare_values(&Value::Bytes(vec![1]), &Value::Bytes(vec![2])),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn hash_aggregate_count_sum_per_group() {
    let scan = boxed_scan(
        vec!["g".into(), "v".into()],
        vec![
            row([("g", Value::Str("a".into())), ("v", Value::Int(1))]),
            row([("g", Value::Str("a".into())), ("v", Value::Int(2))]),
            row([("g", Value::Str("b".into())), ("v", Value::Int(5))]),
        ],
    );
    let agg = HashAggregate::new(
        scan,
        vec![("g".into(), col("g"))],
        vec![
            AggregateSpec {
                kind: AggregateKind::Count,
                arg: Some(col("v")),
                alias: "n".into(),
                distinct: false,
            },
            AggregateSpec {
                kind: AggregateKind::Sum,
                arg: Some(col("v")),
                alias: "total".into(),
                distinct: false,
            },
        ],
        vec![],
    );
    let mut agg = agg;
    let (_cols, rows) = run_to_rows(&mut agg).unwrap();
    assert_eq!(rows.len(), 2);
    let by_group: BTreeMap<String, &ResultRow> = rows
        .iter()
        .map(|r| match &r["g"] {
            Value::Str(s) => (s.clone(), r),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(by_group["a"]["n"], Value::Int(2));
    assert_eq!(by_group["a"]["total"], Value::Float(3.0));
    assert_eq!(by_group["b"]["n"], Value::Int(1));
    assert_eq!(by_group["b"]["total"], Value::Float(5.0));
}

#[test]
fn aggregate_count_finalizer_rejects_bigint_overflow() {
    let mut fold = AggFold::new(1, false);
    fold.count = i64::MAX as u64 + 1;
    let spec = AggregateSpec {
        kind: AggregateKind::CountStar,
        arg: None,
        alias: "count".into(),
        distinct: false,
    };
    assert!(finalise_fold(&fold, &spec)
        .unwrap_err()
        .to_string()
        .contains("exceeds BIGINT"));
}

#[test]
fn hash_aggregate_tiny_budget_spills_input_groups_and_distinct_state() {
    let rows = (0..512_i64)
        .map(|value| row([("g", Value::Int(value % 64)), ("v", Value::Int(value % 17))]))
        .collect();
    let scan = boxed_scan(vec!["g".into(), "v".into()], rows);
    let mut aggregate = HashAggregate::new_with_work_mem(
        scan,
        vec![("g".into(), col("g"))],
        vec![AggregateSpec {
            kind: AggregateKind::Count,
            arg: Some(col("v")),
            alias: "unique_values".into(),
            distinct: true,
        }],
        vec![],
        1,
    );
    let (_, rows) = run_to_rows(&mut aggregate).unwrap();
    assert!(aggregate.output_has_spilled());
    assert_eq!(rows.len(), 64);
    assert!(rows
        .iter()
        .all(|row| { matches!(row.get("unique_values"), Some(Value::Int(count)) if *count > 0) }));
}

#[test]
fn window_row_number_dense_rank() {
    let scan = boxed_scan(
        vec!["g".into(), "v".into()],
        vec![
            row([("g", Value::Str("a".into())), ("v", Value::Int(10))]),
            row([("g", Value::Str("a".into())), ("v", Value::Int(20))]),
            row([("g", Value::Str("a".into())), ("v", Value::Int(20))]),
            row([("g", Value::Str("b".into())), ("v", Value::Int(7))]),
        ],
    );
    let win = Window::new(
        scan,
        WindowSpec {
            partition_by: vec![col("g")],
            order_by: vec![SortKey {
                expr: col("v"),
                descending: false,
                nulls_first: None,
            }],
        },
        vec![
            ("rn".into(), WindowKind::RowNumber),
            ("dr".into(), WindowKind::DenseRank),
        ],
        vec![],
    );
    let mut win = win;
    let (_cols, rows) = run_to_rows(&mut win).unwrap();
    assert_eq!(rows.len(), 4);
    // partition `a` ordered by v ascending: 10, 20, 20.
    let part_a: Vec<&ResultRow> = rows
        .iter()
        .filter(|r| matches!(&r["g"], Value::Str(s) if s == "a"))
        .collect();
    assert_eq!(part_a.len(), 3);
    let row_for_v = |v: i64| -> &ResultRow {
        *part_a
            .iter()
            .find(|r| matches!(r["v"], Value::Int(x) if x == v))
            .unwrap()
    };
    assert_eq!(row_for_v(10)["dr"], Value::Int(1));
    // Two ties on v=20 share a dense rank of 2.
    let twenties: Vec<&&ResultRow> = part_a
        .iter()
        .filter(|r| matches!(r["v"], Value::Int(20)))
        .collect();
    assert_eq!(twenties.len(), 2);
    for r in twenties {
        assert_eq!(r["dr"], Value::Int(2));
    }
}

#[test]
fn window_tiny_budget_uses_disk_partition_for_random_access() {
    let rows = (0..512_i64)
        .map(|value| row([("g", Value::Int(1)), ("v", Value::Int(value))]))
        .collect();
    let scan = boxed_scan(vec!["g".into(), "v".into()], rows);
    let mut window = Window::new_with_work_mem(
        scan,
        WindowSpec {
            partition_by: vec![col("g")],
            order_by: vec![SortKey {
                expr: col("v"),
                descending: false,
                nulls_first: None,
            }],
        },
        vec![
            ("rn".into(), WindowKind::RowNumber),
            ("next".into(), WindowKind::Lead(col("v"), 1)),
            ("total".into(), WindowKind::AggSum(col("v"))),
        ],
        vec![],
        1,
    );
    let (_, rows) = run_to_rows(&mut window).unwrap();
    assert!(window.output_has_spilled());
    assert_eq!(rows.len(), 512);
    assert_eq!(rows[0].get("rn"), Some(&Value::Int(1)));
    assert_eq!(rows[0].get("next"), Some(&Value::Int(1)));
    assert_eq!(rows[511].get("next"), Some(&Value::Null));
    assert_eq!(
        rows[511].get("total"),
        Some(&Value::Float((0..512_i64).sum::<i64>() as f64))
    );
}
