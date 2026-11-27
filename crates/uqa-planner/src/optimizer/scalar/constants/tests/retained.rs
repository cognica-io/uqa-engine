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
        let retained = owned::retain_large_literal(
            uqa_sql::plan::OwnedScalarExpr::uncontrolled(ordinary.clone()),
            crate::optimizer::scalar::ScalarProduction::admitted(&settings),
        )
        .unwrap()
        .finish(&settings.production)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
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
    let factory = |_: &uqa_core::memory::ProductionControl<'_>| {
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
        let factory = |_: &uqa_core::memory::ProductionControl<'_>| {
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
        for query in [&copy, plan.as_ref()] {
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

#[test]
fn values_inputs_reuse_the_enclosing_routine_stack() {
    use crate::optimizer::{optimize, OptimizerConfig};
    use uqa_sql::plan::UnifiedPlan;

    fn allocated(sql: &str, settings: &OptimizerConfig<'_>) -> u64 {
        let plan = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        allocation_counter::measure(|| drop(optimize(plan, settings).unwrap())).bytes_total
    }

    let shallow = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    let stack = vec![[7; 16]; 4096];
    let nested = OptimizerConfig {
        active_inline_routines: &stack,
        ..shallow.clone()
    };
    let mut differences = Vec::new();
    for sql in [
        "VALUES (1), (2)",
        "SELECT * FROM (VALUES (1), (2)) item(n)",
        "INSERT INTO target VALUES (1), (2)",
    ] {
        allocated(sql, &shallow);
        let ordinary = allocated(sql, &shallow);
        let enclosing = allocated(sql, &nested);
        if ordinary != enclosing {
            differences.push((sql, ordinary, enclosing));
        }
    }
    assert!(differences.is_empty(), "{differences:?}");

    for (allowance, cancel, expected) in [
        (0, 0, Some("53200")),
        (0, 1, Some("57014")),
        (0, 2, Some("57014")),
        (128 * 1024, 0, None),
    ] {
        let memory = uqa_core::memory::MemoryBudget::new(allowance);
        let original = uqa_core::CancellationToken::new();
        let invoking = uqa_core::CancellationToken::new();
        let settings = OptimizerConfig {
            production: uqa_core::memory::ProductionControl::new(&memory, &original, &invoking),
            ..nested.clone()
        };
        if cancel == 1 {
            original.cancel();
        } else if cancel == 2 {
            invoking.cancel();
        }
        let mut entered = false;
        let result = settings.with_inline_routine([9; 16], |child| {
            entered = true;
            let (last, parents) = child.active_inline_routines.split_last().unwrap();
            assert_eq!(last, &[9; 16]);
            assert_eq!(parents, stack);
            assert!(memory.used() >= std::mem::size_of_val(child.active_inline_routines));
            let values = child.for_values()?;
            assert!(std::ptr::eq(
                values.active_inline_routines,
                child.active_inline_routines
            ));
            Ok(())
        });
        assert_eq!(
            result.as_ref().err().and_then(uqa_sql::SQLError::sqlstate),
            expected
        );
        assert_eq!(entered, expected.is_none());
        assert_eq!(memory.used(), 0);
    }
}
