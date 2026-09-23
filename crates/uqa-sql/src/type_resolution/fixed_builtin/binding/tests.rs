//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Expr, plan::ExpressionPlan, RowSchema};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn input(name: &str, value: Value, budget: &MemoryBudget) -> call::CallOwner {
    let token = CancellationToken::new();
    let expression = Expr::Func {
        name: name.into(),
        binding: None,
        args: vec![Expr::Literal(value)],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let lowered =
        ExpressionPlan::lower_column_budgeted(&expression, budget, &token, &token).unwrap();
    let (expression, memory) = lowered.into_parts();
    let ScalarExpr::Func {
        name,
        binding,
        args,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        panic!("fixture function")
    };
    call::CallOwner {
        call: BindingCall {
            name,
            binding,
            arguments: args,
            distinct,
            order_by,
            filter,
        },
        memory: Some(memory),
    }
}

#[test]
fn local_fixed_binding_selects_the_same_default_cast_and_durable_signature() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::default();
    let mut infer = |expr: &ScalarExpr| {
        crate::type_resolution::scalar_type_inner_with_control(expr, &schema, &[], None, &control)
    };
    let mut root = input(
        "jsonb_strip_nulls",
        Value::Str("{\"a\":null}".into()),
        &budget,
    );
    let before = budget.used();
    bind_call_in_place_with_control(&mut root.call, &mut root.memory, &[], &mut infer, &control)
        .unwrap();
    let binding = root.call.binding.as_ref().unwrap();
    assert_eq!(binding.name, "pg_catalog.jsonb_strip_nulls");
    assert_eq!(binding.argument_types, ["jsonb", "boolean"]);
    assert!(matches!(&root.call.arguments[0], ScalarExpr::Cast {ty, ..} if ty == "jsonb"));
    assert_eq!(
        root.call.arguments[1],
        ScalarExpr::Literal(Value::Bool(false))
    );
    assert!(budget.used() > before);
    assert_eq!(budget.used(), root.memory.as_ref().unwrap().bytes());
    drop(root);
    assert_eq!(budget.used(), 0);
}

#[test]
fn local_fixed_binding_retains_undefined_function_diagnostics() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut root = input("to_bin", Value::Bool(true), &budget);
    let mut infer = |_: &ScalarExpr| Ok(Some(ColumnType::Boolean.clone_with_control(&control)?));
    bind_call_in_place_with_control(&mut root.call, &mut root.memory, &[], &mut infer, &control)
        .unwrap();
    let error = root
        .call
        .binding
        .as_ref()
        .unwrap()
        .resolution_error
        .as_ref()
        .unwrap()
        .sql_error();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert!(error.to_string().contains("to_bin(boolean)"));
    assert_eq!(budget.used(), root.memory.as_ref().unwrap().bytes());
    drop(root);
    assert_eq!(budget.used(), 0);
}

#[test]
fn local_fixed_binding_preserves_root_lease_on_inference_resource_failure() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for sqlstate in ["53200", "57014"] {
        let mut root = input("jsonb_strip_nulls", Value::Str("{}".into()), &budget);
        let before = budget.used();
        let mut infer = |_: &ScalarExpr| {
            Err(SQLError::Routine {
                sqlstate: sqlstate.into(),
                message: "fixture inference resource failure".into(),
            })
        };
        let error = bind_call_in_place_with_control(
            &mut root.call,
            &mut root.memory,
            &[],
            &mut infer,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate));
        assert_eq!(budget.used(), before);
        assert!(root.call.binding.is_none());
        assert_eq!(root.call.arguments.len(), 1);
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}
