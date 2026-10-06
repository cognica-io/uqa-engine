//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Expr, plan::ExpressionPlan, RowSchema};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn expression(sql: &str) -> Expr {
    let crate::Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("fixture query")
    };
    query.projections[0].expr.clone()
}

#[test]
fn array_inspection_keeps_concrete_arguments_and_polymorphic_identity() {
    use crate::type_resolution::{bind_type_introspection, bind_type_introspection_with_control};

    let schema = RowSchema::default();
    for name in [
        "array_dims",
        "array_ndims",
        "cardinality",
        "array_lower",
        "array_upper",
        "array_length",
    ] {
        let dimension = matches!(name, "array_lower" | "array_upper" | "array_length");
        let sql = format!(
            "SELECT {name}($1{})",
            if dimension { ",1::smallint" } else { "" }
        );
        let expression = expression(&sql);
        for ty in [
            ColumnType::Array(Box::new(ColumnType::Integer)),
            ColumnType::Array(Box::new(ColumnType::Text)),
            ColumnType::Int2Vector,
            ColumnType::OidVector,
            ColumnType::AnyArray,
            ColumnType::Domain {
                schema: "public".into(),
                name: "integer_array".into(),
                oid: 90_001,
                array_oid: None,
                base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
            },
        ] {
            let params = [SQLParam::typed_scalar(Value::Null, ty.clone())];
            let expected = bind_type_introspection(
                ExpressionPlan::lower(expression.clone()).scalar,
                &schema,
                &params,
            );
            let budget = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = uqa_core::memory::ProductionControl::new(&budget, &token, &token);
            let input = ExpressionPlan::lower_column_budgeted(&expression, &budget, &token, &token)
                .unwrap();
            let actual =
                bind_type_introspection_with_control(input.into(), &schema, &params, &control)
                    .unwrap();
            assert_eq!(*actual, expected, "{sql}: {ty:?}");
            let ScalarExpr::Func {
                binding: Some(binding),
                args,
                ..
            } = &*actual
            else {
                panic!("array inspection must retain the selected identity")
            };
            assert_eq!(binding.name, format!("pg_catalog.{name}"));
            assert_eq!(binding.argument_types[0], "anyarray");
            assert_eq!(args[0], ScalarExpr::Param(1));
            assert_eq!(
                scalar_type_inner(&args[0], &schema, &params, None).unwrap(),
                Some(ty.clone())
            );
            if dimension {
                assert_eq!(binding.argument_types[1], "integer");
                assert!(
                    matches!(&args[1], ScalarExpr::Cast { ty, implicit: true, .. } if ty == "integer")
                );
            }
            let mut types = vec![Some(ty.clone())];
            if dimension {
                types.push(Some(ColumnType::SmallInteger));
            }
            let targets = selected_argument_targets(name, &types).unwrap();
            assert_eq!(targets[0], Some(ty));
            assert_eq!(budget.used(), actual.reserved_bytes());
            drop(actual);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn array_inspection_rejects_unknown_scalar_dimension_and_named_inputs() {
    for name in [
        "array_dims",
        "array_ndims",
        "cardinality",
        "array_lower",
        "array_upper",
        "array_length",
    ] {
        let dimension = matches!(name, "array_lower" | "array_upper" | "array_length");
        for (value, state) in [("NULL", "42804"), ("'{1,2}'", "42804"), ("1", "42883")] {
            let sql = format!(
                "SELECT {name}({value}{})",
                if dimension { ",1" } else { "" }
            );
            let scalar = ExpressionPlan::lower(expression(&sql)).scalar;
            let error = crate::scalar_type(&scalar, &RowSchema::default(), &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{sql}");
        }
    }
    for (sql, signature) in [
        (
            "SELECT array_length(ARRAY[1,2],1::bigint)",
            "array_length(integer[], bigint)",
        ),
        (
            "SELECT array_dims(ARRAY[1,2],1)",
            "array_dims(integer[], integer)",
        ),
        (
            "SELECT array_length(a=>ARRAY[1,2],dim=>1)",
            "array_length(a => integer[], dim => integer)",
        ),
    ] {
        let scalar = ExpressionPlan::lower(expression(sql)).scalar;
        let error = crate::scalar_type(&scalar, &RowSchema::default(), &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}");
        assert!(error.to_string().contains(signature), "{sql}: {error}");
    }
}
