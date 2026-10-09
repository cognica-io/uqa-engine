//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::AggregateAccumulatorTemplate;
use uqa_sql::expr::RowLookup as _;

mod enums;
mod source_layout;

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
                    definition: None,
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
    let slots = functions.into_iter().map(prepared).collect();
    let (output, columns, _) = evaluate_slots(rows, order_by, slots, 1 << 20, budget_bytes);
    (output, columns)
}

/// Evaluate one partition of `(k, v)` rows, buffered within `partition_budget` bytes, with the given prepared functions; also return the bytes the partition spilled.
fn evaluate_slots(
    rows: &[(i64, i64)],
    order_by: Vec<ScalarOrder>,
    mut slots: Vec<PreparedSlot>,
    partition_budget: usize,
    budget_bytes: usize,
) -> (SpillBuffer, Vec<uqa_sql::ast::InternalColumnRef>, u64) {
    let hook = NoSequences;
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, None);
    let partition_schema = RowSchema::new(vec!["k".into(), "v".into()]);
    let mut partition = BufferedIndexedSpill::new(partition_schema.clone(), partition_budget);
    for (k, v) in rows {
        partition
            .push(&PhysicalRow::from_values(vec![
                Value::Int(*k),
                Value::Int(*v),
            ]))
            .unwrap();
    }
    let pass = pass(order_by, slots.len());
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
    (output, columns, partition.spilled_bytes())
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
        AggregateAccumulatorTemplate::builtin("sum", None),
        1 << 20,
    )))
}

fn count() -> WindowFunction {
    WindowFunction::Aggregate(Box::new(aggregates::WindowAggregate::new(
        ("count", &[ScalarExpr::Column("v".into())], None),
        AggregateAccumulatorTemplate::builtin("count", None),
        1 << 20,
    )))
}

/// `function` over a frame of `mode` between `start` and `end`, each with its constant offset.
fn framed(
    function: WindowFunction,
    mode: FrameMode,
    (start, start_offset): (BoundKind, Option<i64>),
    (end, end_offset): (BoundKind, Option<i64>),
) -> PreparedSlot {
    let offset = |value: Option<i64>| {
        value.map(|value| FrameOffset {
            expression: ScalarExpr::Literal(Value::Int(value)),
            target: ColumnType::BigInteger,
        })
    };
    PreparedSlot {
        state: WindowFunctionState::new(
            function,
            FrameSpec {
                mode,
                start,
                end,
                ..default_frame()
            },
        ),
        start_offset: offset(start_offset),
        end_offset: offset(end_offset),
    }
}

fn by_v() -> Vec<ScalarOrder> {
    vec![ScalarOrder {
        expr: ScalarExpr::Column("v".into()),
        descending: false,
        nulls: None,
    }]
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
    let (output, columns, spilled) = evaluate_slots(&rows, Vec::new(), vec![prepared(sum())], 0, 1);
    assert!(spilled > 4096 * 8);
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
        WindowFunction::LastValue(crate::scalar::PreparedExpressions::scalar(
            ScalarExpr::Column("v".into()),
        )),
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

/// Each row's `count(v)` and `sum(v)` over a frame of `mode`, with the partition buffered in memory and spilled to disk.
fn frame_counts_and_sums(
    rows: &[(i64, i64)],
    order_by: &[ScalarOrder],
    mode: FrameMode,
    start: (BoundKind, Option<i64>),
    end: (BoundKind, Option<i64>),
) -> Vec<Vec<Vec<Value>>> {
    [0, 65536]
        .into_iter()
        .map(|partition_budget| {
            let slots = vec![
                framed(count(), mode, start, end),
                framed(sum(), mode, start, end),
            ];
            let (output, columns, _) =
                evaluate_slots(rows, order_by.to_vec(), slots, partition_budget, 1 << 20);
            results(output, &columns)
        })
        .collect()
}

fn counts_and_sums(expected: &[(i64, Option<i64>)]) -> Vec<Vec<Value>> {
    expected
        .iter()
        .map(|(count, sum)| vec![Value::Int(*count), sum.map_or(Value::Null, Value::Int)])
        .collect()
}

#[test]
fn rows_frames_intersect_the_partition_without_creating_boundary_rows() {
    // PostgreSQL 18: VALUES (1,10),(2,20),(3,30), ordered by k. Following frames produce counts [2,1,0] and sums [50,30,NULL]; preceding frames produce counts [0,1,2] and sums [NULL,10,30].
    let rows = [(1, 10), (2, 20), (3, 30)];
    let cases = [
        (
            (BoundKind::Following, Some(1)),
            (BoundKind::Following, Some(2)),
            [(2, Some(50)), (1, Some(30)), (0, None)],
        ),
        (
            (BoundKind::Preceding, Some(2)),
            (BoundKind::Preceding, Some(1)),
            [(0, None), (1, Some(10)), (2, Some(30))],
        ),
    ];
    for (start, end, expected) in cases {
        for results in frame_counts_and_sums(&rows, &by_k(), FrameMode::Rows, start, end) {
            assert_eq!(results, counts_and_sums(&expected));
        }
    }
}

#[test]
fn groups_frames_offset_peer_groups_and_preserve_empty_frames() {
    // PostgreSQL 18: VALUES (1,10),(2,10),(3,20),(4,20),(5,30), ordered by v. GROUPS offsets select complete peers rather than individual rows.
    let rows = [(1, 10), (2, 10), (3, 20), (4, 20), (5, 30)];
    let cases = [
        (
            (BoundKind::Preceding, Some(1)),
            (BoundKind::Following, Some(1)),
            [
                (4, Some(60)),
                (4, Some(60)),
                (5, Some(90)),
                (5, Some(90)),
                (3, Some(70)),
            ],
        ),
        (
            (BoundKind::Following, Some(1)),
            (BoundKind::Following, Some(2)),
            [
                (3, Some(70)),
                (3, Some(70)),
                (1, Some(30)),
                (1, Some(30)),
                (0, None),
            ],
        ),
    ];
    for (start, end, expected) in cases {
        for results in frame_counts_and_sums(&rows, &by_v(), FrameMode::Groups, start, end) {
            assert_eq!(results, counts_and_sums(&expected));
        }
    }
}
