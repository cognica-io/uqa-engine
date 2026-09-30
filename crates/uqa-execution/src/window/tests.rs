//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::{AggregateAccumulatorTemplate, AggregateStatePlan};
use uqa_sql::expr::RowLookup as _;

struct NoSequences;

impl uqa_sql::expr::EngineHook for NoSequences {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal("window fixture has no sequences".into()))
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal("window fixture has no sequences".into()))
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        Err(SQLError::Internal("window fixture has no sequences".into()))
    }
}

fn default_frame() -> FrameSpec {
    FrameSpec {
        mode: FrameMode::Range,
        start: BoundKind::UnboundedPreceding,
        end: BoundKind::CurrentRow,
        exclusion: FrameExclusion::NoOthers,
        start_offset: None,
        end_offset: None,
        ascending: true,
        nulls_first: false,
    }
}

fn prepared(function: WindowFunction) -> PreparedSlot {
    PreparedSlot {
        state: WindowFunctionState::new(function, default_frame()),
        start_offset: None,
        end_offset: None,
    }
}

fn pass(order_by: Vec<ScalarOrder>, slots: usize) -> WindowPass {
    WindowPass {
        partition_by: Vec::new(),
        order_by: order_by.clone(),
        slots: (0..slots)
            .map(|_| WindowSlot {
                column: uqa_sql::ast::InternalRelationId::allocate().column(0),
                name: "window".into(),
                args: Vec::new(),
                spec: ScalarWindowSpec {
                    partition_by: Vec::new(),
                    order_by: order_by.clone(),
                    frame: None,
                },
                filter: None,
            })
            .collect(),
    }
}

/// Evaluate one partition of `(k, v)` rows with the given window functions and return each row's results.
fn evaluate(
    rows: &[(i64, i64)],
    order_by: Vec<ScalarOrder>,
    functions: Vec<WindowFunction>,
    budget_bytes: usize,
) -> (SpillBuffer, Vec<uqa_sql::ast::InternalColumnRef>) {
    let hook = NoSequences;
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, None);
    let partition_schema = RowSchema::new(vec!["k".into(), "v".into()]);
    let mut partition = IndexedSpill::new(partition_schema.clone()).unwrap();
    for (k, v) in rows {
        partition
            .push(&PhysicalRow::from_values(vec![
                Value::Int(*k),
                Value::Int(*v),
            ]))
            .unwrap();
    }
    let pass = pass(order_by, functions.len());
    let columns = pass
        .slots
        .iter()
        .map(|slot| slot.column)
        .collect::<Vec<_>>();
    let schema = RowSchema::append_internal_typed(
        &partition_schema,
        &columns
            .iter()
            .map(|column| (*column, None))
            .collect::<Vec<_>>(),
    );
    let mut slots = functions.into_iter().map(prepared).collect::<Vec<_>>();
    let mut output = SpillBuffer::new(budget_bytes);
    let mut offsets_evaluated = false;
    emit_partition(
        &pass,
        &mut slots,
        &mut partition,
        (&schema, &mut output),
        (&[], &hook, &arena),
        &mut offsets_evaluated,
    )
    .unwrap();
    (output, columns)
}

fn results(
    mut output: SpillBuffer,
    columns: &[uqa_sql::ast::InternalColumnRef],
) -> Vec<Vec<Value>> {
    let mut results = Vec::new();
    for batch in output.drain().unwrap() {
        let batch = batch.unwrap();
        for row in &batch.rows {
            let view = batch.schema.view(row);
            results.push(
                columns
                    .iter()
                    .map(|column| view.internal_column(*column).cloned().unwrap())
                    .collect(),
            );
        }
    }
    results
}

fn sum() -> WindowFunction {
    WindowFunction::Aggregate(Box::new(aggregates::WindowAggregate::new(
        ("sum", &[ScalarExpr::Column("v".into())], None),
        AggregateAccumulatorTemplate::Builtin(AggregateStatePlan::Sum),
        1 << 20,
    )))
}

fn by_k() -> Vec<ScalarOrder> {
    vec![ScalarOrder {
        expr: ScalarExpr::Column("k".into()),
        descending: false,
        nulls: None,
    }]
}

#[test]
fn huge_partition_stays_disk_backed_with_a_tiny_output_budget() {
    let rows = (0..4096_i64).map(|id| (id, 1)).collect::<Vec<_>>();
    let (output, columns) = evaluate(&rows, Vec::new(), vec![sum()], 1);
    assert!(output.has_spilled());
    assert!(output.in_memory_bytes() <= output.budget_bytes());
    assert_eq!(output.rows(), 4096);
    let results = results(output, &columns);
    assert!(results.iter().all(|row| row == &[Value::Int(4096)]));
}

#[test]
fn peers_share_the_default_frame_and_ranks() {
    let rows = [(1, 10), (1, 20), (2, 5), (3, 1), (3, 2)];
    let functions = vec![
        sum(),
        WindowFunction::Rank,
        WindowFunction::DenseRank,
        WindowFunction::RowNumber,
        WindowFunction::CumeDist,
        WindowFunction::LastValue(ScalarExpr::Column("v".into())),
    ];
    let (output, columns) = evaluate(&rows, by_k(), functions, 1 << 20);
    let results = results(output, &columns);
    let expected = [
        (30, 1, 1, 1, 0.4, 20),
        (30, 1, 1, 2, 0.4, 20),
        (35, 3, 2, 3, 0.6, 5),
        (38, 4, 3, 4, 1.0, 2),
        (38, 4, 3, 5, 1.0, 2),
    ];
    for (row, (sum, rank, dense, number, cume, last)) in results.iter().zip(expected) {
        assert_eq!(
            row,
            &[
                Value::Int(sum),
                Value::Int(rank),
                Value::Int(dense),
                Value::Int(number),
                Value::Float(cume),
                Value::Int(last),
            ]
        );
    }
}
