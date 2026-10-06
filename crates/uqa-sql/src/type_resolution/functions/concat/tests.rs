//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::ExpressionPlan, RowSchema, ScalarExpr};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn expression(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT expression")
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

#[test]
fn selected_array_operands_preserve_common_types_and_resource_errors() {
    let integer = ColumnType::Array(Box::new(ColumnType::Integer));
    let bigint = ColumnType::Array(Box::new(ColumnType::BigInteger));
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (left, right, operation, expected) in [
        (
            Some(&integer),
            Some(&bigint),
            FunctionDispatch::ArrayConcat,
            &bigint,
        ),
        (
            Some(&integer),
            Some(&ColumnType::BigInteger),
            FunctionDispatch::ArrayAppend,
            &bigint,
        ),
        (
            Some(&ColumnType::BigInteger),
            Some(&integer),
            FunctionDispatch::ArrayPrepend,
            &bigint,
        ),
        (
            Some(&integer),
            None,
            FunctionDispatch::ArrayConcat,
            &integer,
        ),
    ] {
        let (dispatch, result) = array_concat_types(left, right, &control).unwrap().unwrap();
        assert_eq!(dispatch, operation);
        assert_eq!(&*result, expected);
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let error = array_concat_types(Some(&integer), Some(&ColumnType::Text), &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert!(error
        .to_string()
        .contains("operator does not exist: integer[] || text"));
    assert!(error.hint().is_some());
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(1);
    let constrained = ProductionControl::new(&small, &token, &token);
    assert_eq!(
        array_concat_types(Some(&integer), None, &constrained)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        array_concat_types(Some(&integer), None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn bound_array_null_roles_survive_serialization_rebinding_and_controlled_dispatch() {
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (sql, expected) in [
        (
            "NULL::integer[] || NULL::integer",
            FunctionDispatch::ArrayAppend,
        ),
        (
            "NULL::integer || NULL::integer[]",
            FunctionDispatch::ArrayPrepend,
        ),
        ("NULL::integer[] || NULL", FunctionDispatch::ArrayConcat),
    ] {
        let crate::Statement::Select(mut query) =
            crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
        else {
            panic!("SELECT expression")
        };
        let input = ExpressionPlan::lower_column_budgeted(
            &query.projections.remove(0).expr,
            &budget,
            &token,
            &token,
        )
        .unwrap()
        .into();
        let bound = crate::bind_type_introspection_with_control(
            input,
            &RowSchema::default(),
            &[],
            &control,
        )
        .unwrap();
        let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &*bound
        else {
            panic!("bound array operator")
        };
        assert_eq!(binding.dispatch, Some(expected));
        let restored: ScalarExpr =
            serde_json::from_str(&serde_json::to_string(&*bound).unwrap()).unwrap();
        assert_eq!(
            crate::bind_type_introspection(restored, &RowSchema::default(), &[]),
            *bound
        );
        let mut values = uqa_core::memory::ProductionVec::new(control);
        values
            .push_produced(
                control
                    .finish((None, Value::Null), control.empty_reservation())
                    .unwrap(),
            )
            .unwrap();
        values
            .push_produced(
                control
                    .finish((None, Value::Null), control.empty_reservation())
                    .unwrap(),
            )
            .unwrap();
        let values = values.finish().unwrap();
        let output = crate::expr::eval_generated_function_call_with_control(
            "concat_op",
            Some(binding),
            values,
            &control,
        )
        .unwrap();
        if expected == FunctionDispatch::ArrayConcat {
            assert_eq!(*output, Value::Null);
        } else {
            assert_eq!(output.array_view().unwrap().elements(), &[Value::Null]);
        }
        drop(output);
        drop(bound);
        assert_eq!(budget.used(), 0);
    }
    let dynamic = expression("field || ARRAY[1]");
    assert_eq!(
        crate::bind_type_introspection(dynamic.clone(), &RowSchema::default(), &[]),
        dynamic
    );
}
