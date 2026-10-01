//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::RowSchemaExecution;
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

#[test]
fn floating_frame_offset_rejects_invalid_and_out_of_range_values() {
    assert_eq!(float_frame_offset(42.0).unwrap(), 42);
    for value in [
        f64::NAN,
        f64::INFINITY,
        -1.0,
        1.5,
        9_223_372_036_854_775_808.0,
    ] {
        assert!(float_frame_offset(value).is_err(), "{value}");
    }
}

#[test]
fn rows_frames_intersect_the_partition_without_creating_boundary_rows() {
    use uqa_sql::ast::FrameMode;

    let hook = NoSequences;
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, None);
    let args = vec![ScalarExpr::Column("v".into())];
    let spec = ScalarWindowSpec {
        partition_by: Vec::new(),
        order_by: Vec::new(),
        frame: None,
    };
    let offset = |value| Box::new(ScalarExpr::Literal(Value::Int(value)));
    // PostgreSQL 18: VALUES (1,10),(2,20),(3,30), ordered by id. Following frames produce counts [2,1,0] and sums [50,30,NULL]; preceding frames produce counts [0,1,2] and sums [NULL,10,30].
    let cases = [
        (
            ScalarFrameBound::Following(offset(1)),
            ScalarFrameBound::Following(offset(2)),
            [2, 1, 0],
            [Value::Int(50), Value::Int(30), Value::Null],
        ),
        (
            ScalarFrameBound::Preceding(offset(2)),
            ScalarFrameBound::Preceding(offset(1)),
            [0, 1, 2],
            [Value::Null, Value::Int(10), Value::Int(30)],
        ),
    ];
    for budget in [0, 65536] {
        let mut partition =
            BufferedIndexedSpill::new(RowSchema::new(vec!["id".into(), "v".into()]), budget);
        for id in 1..=3 {
            partition
                .push(&PhysicalRow::from_values(vec![
                    Value::Int(id),
                    Value::Int(id * 10),
                ]))
                .unwrap();
        }
        for (start, end, counts, sums) in &cases {
            for index in 0..3 {
                for (name, expected) in [
                    ("count", Value::Int(counts[index])),
                    ("sum", sums[index].clone()),
                ] {
                    assert_eq!(
                        evaluate_spilled_window_frame(
                            name,
                            &args,
                            &spec,
                            &mut partition,
                            index as u64,
                            FrameMode::Rows,
                            start,
                            end,
                            &[],
                            &hook,
                            &arena,
                        )
                        .unwrap(),
                        expected,
                    );
                }
            }
        }
    }
}

#[test]
fn groups_frames_offset_peer_groups_and_preserve_empty_frames() {
    use uqa_sql::ast::FrameMode;

    let hook = NoSequences;
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, None);
    let args = vec![ScalarExpr::Column("v".into())];
    let spec = ScalarWindowSpec {
        partition_by: Vec::new(),
        order_by: vec![ScalarOrder {
            expr: ScalarExpr::Column("v".into()),
            descending: false,
            nulls: None,
        }],
        frame: None,
    };
    let offset = |value| Box::new(ScalarExpr::Literal(Value::Int(value)));
    // PostgreSQL 18: VALUES (1,10),(2,10),(3,20),(4,20),(5,30), ordered by v. GROUPS offsets select complete peers rather than individual rows.
    let cases = [
        (
            ScalarFrameBound::Preceding(offset(1)),
            ScalarFrameBound::Following(offset(1)),
            [4, 4, 5, 5, 3],
            [Some(60), Some(60), Some(90), Some(90), Some(70)],
        ),
        (
            ScalarFrameBound::Following(offset(1)),
            ScalarFrameBound::Following(offset(2)),
            [3, 3, 1, 1, 0],
            [Some(70), Some(70), Some(30), Some(30), None],
        ),
    ];
    for budget in [0, 65536] {
        let mut partition =
            BufferedIndexedSpill::new(RowSchema::new(vec!["id".into(), "v".into()]), budget);
        for (index, value) in [10, 10, 20, 20, 30].into_iter().enumerate() {
            partition
                .push(&PhysicalRow::from_values(vec![
                    Value::Int(index as i64 + 1),
                    Value::Int(value),
                ]))
                .unwrap();
        }
        for (start, end, counts, sums) in &cases {
            for index in 0..5 {
                for (name, expected) in [
                    ("count", Value::Int(counts[index])),
                    ("sum", sums[index].map_or(Value::Null, Value::Int)),
                ] {
                    assert_eq!(
                        evaluate_spilled_window_frame(
                            name,
                            &args,
                            &spec,
                            &mut partition,
                            index as u64,
                            FrameMode::Groups,
                            start,
                            end,
                            &[],
                            &hook,
                            &arena,
                        )
                        .unwrap(),
                        expected,
                    );
                }
            }
        }
    }
}

#[test]
fn huge_partition_stays_disk_backed_with_a_tiny_output_budget() {
    let hook = NoSequences;
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, None);
    let partition_schema = RowSchema::new(vec!["id".into(), "v".into()]);
    let mut partition = BufferedIndexedSpill::new(partition_schema.clone(), 0);
    for id in 0..4096_i64 {
        partition
            .push(&crate::PhysicalRow::from_values(vec![
                Value::Int(id),
                Value::Int(1),
            ]))
            .unwrap();
    }
    assert_eq!(partition.len(), 4096);
    assert!(partition.spilled_bytes() > 4096 * 8);

    let slot = WindowSlot {
        column: uqa_sql::ast::InternalRelationId::allocate().column(0),
        name: "sum".into(),
        args: vec![ScalarExpr::Column("v".into())],
        spec: ScalarWindowSpec {
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: None,
        },
    };
    let schema = RowSchema::append_internal_typed(&partition_schema, &[(slot.column, None)]);
    let mut output = SpillBuffer::new(1);
    emit_window_partition(
        &slot,
        &mut partition,
        &schema,
        &mut output,
        &[],
        &hook,
        &arena,
    )
    .unwrap();

    assert!(output.has_spilled());
    assert!(output.in_memory_bytes() <= output.budget_bytes());
    assert_eq!(output.rows(), 4096);
    for batch in output.drain().unwrap() {
        let batch = batch.unwrap();
        for row in &batch.rows {
            assert_eq!(
                batch.schema.view(row).internal_column(slot.column),
                Some(&Value::Int(4096))
            );
        }
    }
}
