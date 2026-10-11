//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Both operand declarations select containment before runtime text erases type identity.

use super::*;
use crate::{ast::RangeFunctionOperation, plan::ExpressionPlan};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn containment_binding_and_diagnostics_match_postgresql_in_both_ownership_modes() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../../expr/scalar_range/tests/elements.json")).unwrap();
    let memory = MemoryBudget::new(32 * 1024);
    let token = CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&memory, &token, &token);
    let schema = RowSchema::default();
    for case in reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["owner"] == true || case["binding"] == true)
    {
        let sql = case["sql"].as_str().unwrap();
        let crate::Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("SELECT reference: {sql}");
        };
        for (column, projection) in query.projections.iter().enumerate() {
            let expression = ExpressionPlan::lower(projection.expr.clone()).scalar;
            let ordinary = bind_type_introspection(expression, &schema, &[]);
            let input =
                ExpressionPlan::lower_column_budgeted(&projection.expr, &memory, &token, &token)
                    .unwrap()
                    .into();
            let controlled =
                bind_type_introspection_with_control(input, &schema, &[], &control).unwrap();
            assert_eq!(*controlled, ordinary, "{sql}");
            let serialized = serde_json::to_string(&ordinary).unwrap();
            let restored: ScalarExpr = serde_json::from_str(&serialized).unwrap();
            assert_eq!(bind_type_introspection(restored, &schema, &[]), ordinary);
            let ordinary_type = scalar_type(&ordinary, &schema, &[]);
            let controlled_type = scalar_type_with_control(&controlled, &schema, &[], &control);
            if matches!(case["error"]["sqlstate"].as_str(), Some("42883" | "42725")) {
                for error in [ordinary_type.unwrap_err(), controlled_type.unwrap_err()] {
                    assert_eq!(
                        error.sqlstate(),
                        case["error"]["sqlstate"].as_str(),
                        "{sql}"
                    );
                    assert_eq!(error.to_string(), case["error"]["message"], "{sql}");
                    assert_eq!(error.detail(), case["error"]["detail"].as_str(), "{sql}");
                    assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{sql}");
                }
            } else {
                assert_eq!(ordinary_type.unwrap(), Some(ColumnType::Boolean), "{sql}");
                assert_eq!(
                    controlled_type.unwrap().as_deref(),
                    Some(&ColumnType::Boolean),
                    "{sql}"
                );
                if case["owner"] == true {
                    let ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } = &ordinary
                    else {
                        panic!("typed containment: {sql}");
                    };
                    let expected = if column == 0 {
                        RangeFunctionOperation::ContainsElement
                    } else {
                        RangeFunctionOperation::ElementContainedBy
                    };
                    assert!(
                        matches!(binding.dispatch, Some(FunctionDispatch::Range {operation, ..}) if operation == expected),
                        "{sql}"
                    );
                }
            }
            drop(controlled);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn retained_range_only_binding_is_corrected_from_declared_element_type() {
    let crate::Statement::Select(query) = crate::compile("SELECT '[1,4)'::int4range @> 2")
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT reference");
    };
    let expression = ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    let schema = RowSchema::default();
    let expected = bind_type_introspection(expression, &schema, &[]);
    let mut legacy = expected.clone();
    let ScalarExpr::Func { name, binding, .. } = &mut legacy else {
        panic!("containment");
    };
    let old = FunctionBinding::dispatched(FunctionDispatch::Range {
        operation: RangeFunctionOperation::Contains,
        subtype: crate::ast::RangeSubtype::Integer,
        multirange: false,
    });
    *name = old.name.clone();
    *binding = Some(old);
    let restored: ScalarExpr =
        serde_json::from_str(&serde_json::to_string(&legacy).unwrap()).unwrap();
    let rebound = bind_type_introspection(restored, &schema, &[]);
    let ScalarExpr::Func { binding, .. } = &rebound else {
        panic!("containment");
    };
    assert!(matches!(
        binding.as_ref().unwrap().dispatch,
        Some(FunctionDispatch::Range {
            operation: RangeFunctionOperation::ContainsElement,
            ..
        })
    ));
    assert_eq!(
        scalar_type(&rebound, &schema, &[]).unwrap(),
        Some(ColumnType::Boolean)
    );
    let mut invalid = rebound;
    let ScalarExpr::Func { args, .. } = &mut invalid else {
        unreachable!();
    };
    args[1] = ScalarExpr::Cast {
        implicit: false,
        expr: Box::new(ScalarExpr::Literal(Value::Null)),
        ty: "text".into(),
    };
    assert_eq!(
        scalar_type(&invalid, &schema, &[]).unwrap_err().sqlstate(),
        Some("42883")
    );
}
