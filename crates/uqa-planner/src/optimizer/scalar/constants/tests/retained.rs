//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

fn repeated() -> ScalarExpr {
    let mut call = bound_call(
        "repeat",
        vec![
            ScalarExpr::Literal(Value::Str("x".into())),
            ScalarExpr::Literal(Value::Int(131_072)),
        ],
    );
    let ScalarExpr::Func { binding, .. } = &mut call else {
        unreachable!()
    };
    *binding = Some(
        uqa_sql::resolve_fixed_builtin_call(
            "repeat",
            None,
            &[None, None],
            &[Some(ColumnType::Text), Some(ColumnType::Integer)],
            false,
            None,
        )
        .unwrap()
        .unwrap()
        .selected
        .binding,
    );
    call
}

fn config() -> crate::optimizer::OptimizerConfig<'static> {
    let mut config = crate::optimizer::OptimizerConfig::new(
        uqa_execution::scalar::eval_constant_scalar_with_control,
    );
    config.constant_storage = Some(Arc::new(uqa_execution::scalar::RetainedConstants::new(
        4096,
    )));
    config.coerced_conditionals = true;
    config.retain_values_constants = true;
    config
}

#[test]
fn folded_values_share_frozen_payloads_and_keep_lazy_conditional_errors() {
    let settings = config();
    let mut rows = Vec::new();
    for _ in 0..128 {
        let mut expression = repeated();
        crate::optimizer::optimize_scalar_expression(&mut expression, &settings).unwrap();
        assert!(matches!(expression, ScalarExpr::RetainedLiteral(_)));
        rows.push(expression);
    }
    drop(settings);
    for expression in &rows {
        assert_eq!(
            uqa_execution::scalar::eval_constant_scalar(expression).unwrap(),
            Value::Str("x".repeat(131_072))
        );
        assert_eq!(
            scalar_type(expression, &RowSchema::default(), &[]).unwrap(),
            Some(ColumnType::Text)
        );
    }
    let division = ScalarExpr::Binary {
        op: BinaryOp::Divide,
        lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
    };
    let mut expression = bound_call(
        "coalesce",
        vec![
            rows.remove(0),
            ScalarExpr::Cast {
                expr: Box::new(division.clone()),
                ty: "text".into(),
                implicit: false,
            },
        ],
    );
    crate::optimizer::optimize_scalar_expression(&mut expression, &config()).unwrap();
    assert_eq!(
        uqa_execution::scalar::eval_constant_scalar(&expression).unwrap(),
        Value::Str("x".repeat(131_072))
    );
    assert_eq!(
        crate::optimizer::optimize_scalar_expression(&mut division.clone(), &config())
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
}

#[test]
fn retained_values_preserve_raw_arrays_declared_names_and_parameter_origin() {
    let settings = config();
    let mut expression = ScalarExpr::Array(vec![repeated(), repeated()]);
    crate::optimizer::optimize_scalar_expression(&mut expression, &settings).unwrap();
    let ScalarExpr::RetainedLiteral(literal) = &expression else {
        panic!("large array must share storage")
    };
    assert!(literal.declaration.is_none());
    let value = uqa_execution::scalar::eval_constant_scalar(&expression).unwrap();
    assert_eq!(
        value.array_view().unwrap().elements(),
        vec![Value::Str("x".repeat(131_072)); 2].as_slice()
    );
    for parameter_index in [None, Some(1)] {
        let ordinary = ScalarExpr::TypedLiteral {
            value: Value::Str("x".repeat(1024)),
            ty: "\"pg_catalog\".\"text\"".into(),
            bound_type: Some(ColumnType::Text),
            parameter_index,
            collation_oid: Some(950),
            composite_source: None,
        };
        let retained =
            retain_large_literal(ordinary.clone(), settings.constant_storage.as_deref()).unwrap();
        assert_eq!(
            serde_json::to_vec(&retained).unwrap(),
            serde_json::to_vec(&ordinary).unwrap()
        );
        assert_eq!(
            uqa_execution::scalar::eval_constant_scalar(&retained).unwrap(),
            uqa_execution::scalar::eval_constant_scalar(&ordinary).unwrap()
        );
        assert_eq!(
            matches!(retained, ScalarExpr::RetainedLiteral(_)),
            parameter_index.is_none()
        );
    }
}

#[test]
fn retained_constant_storage_follows_values_inputs_in_commands_and_queries() {
    let calls = std::cell::Cell::new(0);
    let factory = || {
        calls.set(calls.get() + 1);
        Ok(None)
    };
    let mut settings = crate::optimizer::OptimizerConfig::new(
        uqa_execution::scalar::eval_constant_scalar_with_control,
    );
    settings.constant_storage_factory = Some(crate::optimizer::ConstantStorageFactory(&factory));
    for (sql, expected) in [
        ("SELECT 1", 0),
        ("INSERT INTO target SELECT 1", 0),
        ("INSERT INTO target DEFAULT VALUES", 0),
        ("INSERT INTO target VALUES (1), (2)", 1),
        ("VALUES (1), (2)", 1),
        ("SELECT * FROM (VALUES (1), (2)) AS input(n)", 1),
        (
            "INSERT INTO target SELECT * FROM (VALUES (1), (2)) AS input(n)",
            1,
        ),
        (
            "WITH inserted AS (INSERT INTO target VALUES (1) RETURNING id) SELECT * FROM inserted",
            1,
        ),
    ] {
        calls.set(0);
        let plan = uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        crate::optimizer::optimize(plan, &settings).unwrap();
        assert_eq!(calls.get(), expected, "{sql}");
    }
}

#[test]
fn values_queries_bound_folded_payloads_before_cloning_their_native_plans() {
    let values = std::iter::repeat_n("(repeat('x',131072))", 128)
        .collect::<Vec<_>>()
        .join(",");
    for sql in [
        format!("VALUES {values}"),
        format!("SELECT n FROM (VALUES {values}) AS input(n)"),
        format!("INSERT INTO target SELECT n FROM (VALUES {values}) AS input(n)"),
    ] {
        let storage = Arc::new(uqa_execution::scalar::RetainedConstants::new(4096));
        let factory = || {
            Ok(Some(
                storage.clone() as Arc<dyn uqa_sql::ir::ConstantValueStorage>
            ))
        };
        let mut settings = crate::optimizer::OptimizerConfig::new(
            uqa_execution::scalar::eval_constant_scalar_with_control,
        );
        settings.constant_storage_factory =
            Some(crate::optimizer::ConstantStorageFactory(&factory));
        let mut input =
            uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0));
        input.rewrite_scalar_expressions(&mut |expression| {
            if matches!(expression, ScalarExpr::Func { name, .. } if name == "repeat") {
                *expression = repeated();
            }
        });
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            let plan = crate::optimizer::optimize(input, &settings).unwrap();
            output = Some((plan.clone(), plan));
        });
        assert!(allocation.bytes_max < 8 * 1024 * 1024, "{allocation:?}");
        let (copy, plan) = output.unwrap();
        for query in [copy, plan] {
            let mut count = 0;
            query.visit_scalar_expressions(&mut |expression| {
                expression.visit(&mut |scalar| {
                    if let ScalarExpr::RetainedLiteral(_) = scalar {
                        assert_eq!(
                            uqa_execution::scalar::eval_constant_scalar(scalar).unwrap(),
                            Value::Str("x".repeat(131_072)),
                        );
                        count += 1;
                    }
                });
            });
            assert_eq!(count, 128);
        }
    }
}
