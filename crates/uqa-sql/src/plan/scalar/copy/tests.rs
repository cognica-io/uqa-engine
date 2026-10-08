//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::ColumnType, plan::ExpressionPlan, ScalarExpr};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn admitted_scalar_copy_preserves_window_frames_bindings_and_typed_payloads() {
    let crate::Statement::Select(mut query) = crate::compile("SELECT CASE WHEN a > 1 THEN sum(a) FILTER (WHERE b IS NOT NULL) OVER (PARTITION BY a ORDER BY b ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING) ELSE 4 END").unwrap().remove(0) else { panic!("SELECT") };
    let window = ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    let source = ScalarExpr::Row(vec![
        window,
        ScalarExpr::TypedLiteral {
            composite_source: None,
            value: Value::Str("retained scalar payload".repeat(100)),
            ty: "character varying[]".into(),
            bound_type: Some(ColumnType::Array(Box::new(ColumnType::Varchar(Some(32))))),
            parameter_index: Some(1),
        },
        ScalarExpr::CompositeRow {
            items: vec![ScalarExpr::Literal(Value::Int(1))],
            binding: crate::ast::CompositeRowBinding {
                argument_types: Some(vec![ColumnType::Integer]),
                ty: "composite#20001".into(),
                attributes: vec![1],
            },
            bound_type: Some(ColumnType::Composite(crate::ast::CompositeTypeReference {
                schema: "catalog_schema".repeat(100),
                name: "pair".repeat(100),
                oid: 20_001,
                array_oid: 20_002,
                relation_oid: 20_003,
            })),
        },
        ScalarExpr::ScalarSubquery(7),
    ]);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.clone_with_control(&control).unwrap();
    assert_eq!(*copied, source);
    assert!(budget.used() > 2_000);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(2_000);
    let control = ProductionControl::new(&small, &token, &token);
    assert!(source.clone_with_control(&control).is_err());
    assert_eq!(small.used(), 0);
    token.cancel();
    assert!(source.clone_with_control(&control).is_err());
    assert_eq!(small.used(), 0);
}

#[test]
fn composite_original_payload_is_retained_through_json_and_budgeted_plan_copies() {
    use crate::expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
    };
    let original = CompositeConstantSource {
        value: Value::Record(vec![
            ("a".into(), Value::Str("A".into())),
            ("dropped".into(), Value::Str("retained".repeat(1024))),
        ]),
        descriptors: vec![CompositeTypeDescriptor {
            type_oid: 20_001,
            relation_oid: 20_003,
            attributes: vec![
                CompositeAttribute {
                    name: "a".into(),
                    ty: ColumnType::InternalChar,
                    number: 1,
                },
                CompositeAttribute {
                    name: "dropped".into(),
                    ty: ColumnType::Text,
                    number: 2,
                },
            ],
        }],
    };
    let expression = crate::ast::Expr::TypedLiteral {
        value: Value::Record(vec![("a".into(), Value::Bool(true))]),
        ty: "composite#20001".into(),
        composite_source: Some(Box::new(original)),
    };
    let restored: crate::ast::Expr =
        serde_json::from_slice(&serde_json::to_vec(&expression).unwrap()).unwrap();
    assert_eq!(restored, expression);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let lowered =
        ExpressionPlan::lower_column_budgeted(&restored, &budget, &token, &token).unwrap();
    assert_eq!(*lowered, ExpressionPlan::lower(expression.clone()).scalar);
    let copied = lowered
        .clone_with_control(&ProductionControl::new(&budget, &token, &token))
        .unwrap();
    assert_eq!(*copied, *lowered);
    assert!(budget.used() >= 2 * 8192);
    drop(copied);
    drop(lowered);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(4096);
    assert!(expression.reserve_column_payload(&small, &token).is_err());
    assert_eq!(small.used(), 0);
    assert!(ExpressionPlan::lower_column_budgeted(&restored, &small, &token, &token).is_err());
    assert_eq!(small.used(), 0);
    let scalar = ExpressionPlan::lower(restored).scalar;
    assert!(scalar
        .clone_with_control(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
}
