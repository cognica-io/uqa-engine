//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::ColumnType;

fn parsed(source: &str) -> ScalarExpr {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("scalar SELECT");
    };
    uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

#[test]
fn projected_predicate_payloads_admit_before_copy_and_retain_only_live_nodes() {
    let schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Text)]);
    let payload = "x".repeat(4 << 20);
    for parameter in [false, true] {
        let expression = ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Column("x".into())),
            rhs: Box::new(if parameter {
                ScalarExpr::Param(1)
            } else {
                ScalarExpr::Literal(Value::Str(payload.clone()))
            }),
        };
        let params = [SQLParam::Scalar(Value::Str(payload.clone()))];
        for bind in [false, true] {
            let memory = MemoryBudget::new(64 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let compile = || {
                if bind {
                    ProjectedPredicate::compile_with_schema_with_control(
                        &expression,
                        &schema,
                        &params,
                        &control,
                    )
                } else {
                    ProjectedPredicate::compile_bound_with_control(
                        &expression,
                        &schema,
                        &params,
                        &control,
                    )
                }
            };
            let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
            let mut result = None;
            let allocation = allocation_counter::measure(|| result = Some(compile()));
            assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
            assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
            assert_eq!(memory.used(), occupied.bytes());
            drop(occupied);
            for token in [&original, &invoking] {
                token.cancel();
                assert_eq!(compile().err().unwrap().sqlstate(), Some("57014"));
                assert_eq!(memory.used(), 0);
                token.reset();
            }
            let mut result = None;
            let allocation =
                allocation_counter::measure(|| result = Some(compile().unwrap().unwrap()));
            let predicate = result.unwrap();
            assert_eq!(allocation.bytes_current as usize, memory.used());
            assert!(memory.used() >= payload.len());
            assert!(predicate.keep(&[&Value::Str(payload.clone())]).unwrap());
            assert!(!predicate.keep(&[&Value::Null]).unwrap());
            drop(predicate);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn projected_specialization_retires_replaced_and_unsupported_partial_trees() {
    let schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Integer)]);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let expression = parsed("x >= 2 AND x <= 4");
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(
            ProjectedPredicate::compile_with_schema_with_control(
                &expression,
                &schema,
                &[],
                &control,
            )
            .unwrap()
            .unwrap(),
        );
    });
    let predicate = result.unwrap();
    assert_eq!(allocation.bytes_current as usize, memory.used());
    assert!(
        matches!(&predicate.expression, ProjectedExpr::IntFieldConjunction(items) if items.len() == 1)
    );
    for (value, expected) in [
        (Value::Int(1), false),
        (Value::Int(2), true),
        (Value::Int(4), true),
        (Value::Int(5), false),
        (Value::Null, false),
    ] {
        assert_eq!(predicate.keep(&[&value]).unwrap(), expected);
    }
    drop(predicate);
    assert_eq!(memory.used(), 0);
    let expression = ScalarExpr::And(vec![
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
        ScalarExpr::ScalarSubquery(0),
    ]);
    // The unsupported query is rejected by the local-input inspection before copying the preceding literal.
    let allocation = allocation_counter::measure(|| {
        assert!(ProjectedPredicate::compile_bound_with_control(
            &expression,
            &schema,
            &[],
            &control
        )
        .unwrap()
        .is_none());
    });
    assert_eq!(allocation.bytes_total, 0);
    let text_schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Text)]);
    let partial = ScalarExpr::And(vec![
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Column("x".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)))),
        },
        ScalarExpr::Default,
    ]);
    let allocation = allocation_counter::measure(|| {
        assert!(ProjectedPredicate::compile_bound_with_control(
            &partial,
            &text_schema,
            &[],
            &control
        )
        .unwrap()
        .is_none());
    });
    assert!(allocation.bytes_total >= 4 << 20);
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(memory.used(), 0);
    let unsupported = parsed("x > 0 AND abs(x) > 0");
    assert!(ProjectedPredicate::compile_with_schema_with_control(
        &unsupported,
        &schema,
        &[],
        &control
    )
    .unwrap()
    .is_none());
    assert_eq!(memory.used(), 0);
}

#[test]
fn projected_pattern_folding_keeps_input_and_compiled_pattern_admission() {
    let payload = "x".repeat(1 << 20);
    let expression = parsed(&format!("x LIKE '{payload}#_%' ESCAPE '#'"));
    let schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Text)]);
    let memory = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(ProjectedPredicate::compile_with_schema_with_control(
            &expression,
            &schema,
            &[],
            &control,
        ));
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    let predicate =
        ProjectedPredicate::compile_with_schema_with_control(&expression, &schema, &[], &control)
            .unwrap()
            .unwrap();
    assert!(memory.used() >= payload.len());
    assert!(predicate
        .keep(&[&Value::Str(format!("{payload}_tail"))])
        .unwrap());
    assert!(!predicate
        .keep(&[&Value::Str(format!("{payload}tail"))])
        .unwrap());
    drop(predicate);
    assert_eq!(memory.used(), 0);
}
