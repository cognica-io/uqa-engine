//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    HashJoin, NestedLoopJoin, RowSchemaExecution, ScalarEvalContext, ScalarExpr, TableScan,
};
use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, JoinKind};
use uqa_sql::expr::RowLookup;
use uqa_sql::{ColumnType, ResultRow};

fn scan(name: &str, values: Vec<Value>) -> TableScan {
    TableScan::from_physical_rows(
        RowSchema::with_qualified_types(name, vec!["k".into()], vec![Some(ColumnType::Integer)]),
        values
            .into_iter()
            .map(|value| PhysicalRow::from_values(vec![value]))
            .collect(),
    )
}

fn key(name: &str) -> ScalarExpr {
    ScalarExpr::QualifiedColumn {
        qualifier: name.into(),
        column: "k".into(),
    }
}

fn whole_rows(operator: &mut dyn PhysicalOperator) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    for batch in crate::physical::run_to_batches(operator).unwrap() {
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let context = ScalarEvalContext::from_row_lookup(&view, &[]);
            rows.push(
                ["l", "r"]
                    .map(|name| {
                        crate::eval_scalar(&ScalarExpr::QualifiedStar(name.into()), &context)
                            .unwrap()
                    })
                    .into(),
            );
        }
    }
    rows.sort();
    rows
}

#[test]
fn outer_join_records_preserve_presence_in_hash_and_nested_spills() {
    let record = |value| Value::Record(vec![("k".into(), Value::Int(value))].into());
    for kind in [JoinKind::Left, JoinKind::Right, JoinKind::Full] {
        let mut expected = vec![vec![record(2), record(2)]];
        if matches!(kind, JoinKind::Left | JoinKind::Full) {
            expected.push(vec![record(1), Value::Null]);
        }
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            expected.push(vec![Value::Null, record(3)]);
        }
        expected.sort();
        for budget in [1, 1 << 20] {
            let inputs = || {
                (
                    Box::new(scan("l", vec![Value::Int(1), Value::Int(2)])),
                    Box::new(scan("r", vec![Value::Int(2), Value::Int(3)])),
                )
            };
            let (left, right) = inputs();
            let mut hash = HashJoin::new_with_work_mem_and_predicate(
                left,
                right,
                kind,
                vec![key("l")],
                vec![key("r")],
                None,
                crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
                ResultRow::new(),
                ResultRow::new(),
                budget,
            );
            assert_eq!(whole_rows(&mut hash), expected);
            assert_eq!(hash.right_input_has_spilled(), budget == 1);
            let (left, right) = inputs();
            let predicate = ScalarExpr::Binary {
                lhs: Box::new(key("l")),
                op: BinaryOp::Equal,
                rhs: Box::new(key("r")),
            };
            let mut nested = NestedLoopJoin::new_with_work_mem(
                left,
                right,
                kind,
                Some(predicate),
                crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
                ResultRow::new(),
                ResultRow::new(),
                budget,
            );
            assert_eq!(whole_rows(&mut nested), expected);
            assert_eq!(nested.right_input_has_spilled(), budget == 1);
        }
    }
}

#[test]
fn a_matched_null_valued_row_is_not_a_null_extended_row() {
    for matched in [false, true] {
        let mut operator = NestedLoopJoin::new_with_work_mem(
            Box::new(scan("l", vec![Value::Int(1)])),
            Box::new(scan("r", vec![Value::Null])),
            JoinKind::Left,
            Some(ScalarExpr::Literal(Value::Bool(matched))),
            crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
            ResultRow::new(),
            ResultRow::new(),
            1,
        );
        let expected = if matched {
            Value::Record(vec![("k".into(), Value::Null)].into())
        } else {
            Value::Null
        };
        assert_eq!(whole_rows(&mut operator)[0][1], expected);
        assert!(operator.right_input_has_spilled());
    }
}

#[test]
fn marking_a_wide_input_shares_its_payload_without_allocation() {
    let schema =
        RowSchema::with_qualified_types("r", vec!["body".into()], vec![Some(ColumnType::Text)]);
    let presence = NullableRow::new(&schema, true);
    let mut input = Some(OwnedPhysicalRow::new(
        schema.clone(),
        PhysicalRow::from_values(vec![Value::Str("x".repeat(256 * 1024))]),
    ));
    let mut marked = None;
    let allocation = allocation_counter::measure(|| {
        marked = Some(presence.owned_row(input.take().unwrap()).unwrap());
    });
    assert_eq!(allocation.bytes_total, 0);
    let marked = marked.unwrap();
    let view = presence.schema.view(&marked);
    let Some(Value::Str(body)) = view.column("body") else {
        panic!("expected retained body")
    };
    assert_eq!(body.len(), 256 * 1024);
}

#[test]
fn nullable_rows_relayout_existing_presence_before_adding_a_marker() {
    let original = scan("r", vec![]).row_schema().clone();
    let first = InternalRelationId::allocate().column(0);
    let second = InternalRelationId::allocate().column(0);
    let source = original.with_nullable_relations(first).unwrap();
    let target = original.with_nullable_relations(second).unwrap();
    let presence = NullableRow::new(&target, true);
    for marker in [Value::Null, Value::Bool(true)] {
        let row = presence
            .owned_row(OwnedPhysicalRow::new(
                source.clone(),
                PhysicalRow::from_values(vec![Value::Null, marker.clone()]),
            ))
            .unwrap();
        let view = presence.schema.view(&row);
        let value = crate::eval_scalar(
            &ScalarExpr::QualifiedStar("r".into()),
            &ScalarEvalContext::from_row_lookup(&view, &[]),
        )
        .unwrap();
        assert_eq!(
            value,
            if marker == Value::Null {
                Value::Null
            } else {
                Value::Record(vec![("k".into(), Value::Null)].into())
            }
        );
    }
    let missing_marker =
        OwnedPhysicalRow::new(original, PhysicalRow::from_values(vec![Value::Null]));
    assert!(presence.owned_row(missing_marker).is_err());
}

#[test]
fn malformed_whole_row_markers_are_not_treated_as_absent_rows() {
    let input = scan("r", vec![]).row_schema().clone();
    let presence = NullableRow::new(&input, true);
    for row in [
        vec![Value::Null],
        vec![Value::Null, Value::Bool(false)],
        vec![Value::Null, Value::Int(1)],
    ] {
        let row = PhysicalRow::from_values(row);
        let view = presence.schema.view(&row);
        let error = crate::eval_scalar(
            &ScalarExpr::QualifiedStar("r".into()),
            &ScalarEvalContext::from_row_lookup(&view, &[]),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("presence for r is unavailable or invalid"));
    }
}

#[test]
fn output_batches_keep_nullable_schema_owners_after_the_operator_retires() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut operator =
        input_with_control(Box::new(scan("r", vec![Value::Null])), true, &control).unwrap();
    operator.open().unwrap();
    let batch = operator.next().unwrap().unwrap();
    operator.close().unwrap();
    drop(operator);
    assert!(
        memory.used() > 0,
        "the result schema must retain its admitted metadata"
    );
    let view = batch.schema.view(&batch.rows[0]);
    assert_eq!(
        crate::eval_scalar(
            &ScalarExpr::QualifiedStar("r".into()),
            &ScalarEvalContext::from_row_lookup(&view, &[])
        )
        .unwrap(),
        Value::Record(vec![("k".into(), Value::Null)].into())
    );
    drop(batch);
    assert_eq!(memory.used(), 0);
}
