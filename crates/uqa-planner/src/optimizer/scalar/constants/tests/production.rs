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

fn repeated(bytes: i64) -> ScalarExpr {
    let mut expression = bound_call(
        "pg_catalog.repeat",
        vec![
            ScalarExpr::Literal(Value::Str("x".into())),
            ScalarExpr::Literal(Value::Int(bytes)),
        ],
    );
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut expression
    else {
        unreachable!()
    };
    binding.argument_types = vec!["text".into(), "integer".into()];
    expression
}

#[test]
fn constant_production_rejects_before_allocating_large_output() {
    for (bytes, cancelled) in [
        (0, None),
        (1024, None),
        (usize::MAX, Some(false)),
        (usize::MAX, Some(true)),
    ] {
        let mut expression = repeated(4 * 1024 * 1024);
        let memory = MemoryBudget::new(bytes);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if let Some(original_owner) = cancelled {
            if original_owner {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let mut config =
            crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        config.production = ProductionControl::new(&memory, &original, &invoking);
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(crate::optimizer::scalar::optimize_scalar_slot(
                &mut expression,
                &config,
            ));
        });
        assert!(
            allocated.bytes_total < 16 * 1024,
            "bytes={bytes}, cancelled={cancelled:?}: {allocated:?}"
        );
        let error = result.unwrap().unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn produced_constant_clones_share_payload_metadata_and_live_allowance() {
    let memory = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let mut config =
        crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = ProductionControl::new(&memory, &token, &token);
    let mut expression = repeated(4 * 1024 * 1024);
    crate::optimizer::optimize_scalar_expression(&mut expression, &config).unwrap();
    let retained = memory.used();
    assert!((4 * 1024 * 1024..4 * 1024 * 1024 + 4096).contains(&retained));
    let mut copies = None;
    let allocations = allocation_counter::measure(|| {
        copies = Some(std::array::from_fn::<_, 64, _>(|_| expression.clone()));
    });
    assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    assert_eq!(memory.used(), retained);
    drop(expression);
    let copies = copies.unwrap();
    assert_eq!(memory.used(), retained);
    let ScalarExpr::RetainedLiteral(literal) = &copies[0] else {
        panic!("shared output")
    };
    let Some(Value::Str(text)) = literal.value.resident_value() else {
        panic!("resident value")
    };
    assert_eq!(text.len(), 4 * 1024 * 1024);
    assert_eq!(
        scalar_type(&copies[0], &RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Text)
    );
    let full = memory.reserve(memory.available()).unwrap();
    let mut next = repeated(1024);
    assert_eq!(
        crate::optimizer::optimize_scalar_expression(&mut next, &config)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(full);
    assert_eq!(memory.used(), retained);
    drop(copies);
    assert_eq!(memory.used(), 0);
}

#[test]
fn nested_constant_folding_releases_intermediate_values_and_preserves_lazy_errors() {
    let memory = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let mut config =
        crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = ProductionControl::new(&memory, &token, &token);
    config.coerced_conditionals = true;
    for (sql, expected, state) in [
        (
            "SELECT length(repeat('x', 1048576))",
            Some(Value::Int(1_048_576)),
            None,
        ),
        (
            "SELECT CASE WHEN true THEN length(repeat('x', 1024)) ELSE 1 / 0 END",
            Some(Value::Int(1024)),
            None,
        ),
        ("SELECT 1 / 0", None, Some("22012")),
    ] {
        let mut plan = uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |expression| {
            *expression = uqa_sql::bind_type_introspection(
                std::mem::replace(expression, ScalarExpr::Literal(Value::Null)),
                &RowSchema::default(),
                &[],
            );
        });
        let result = crate::optimizer::optimize(plan, &config);
        assert_eq!(
            result.as_ref().err().and_then(|error| match error {
                crate::optimizer::OptimizerError::Expression(error) => error.sqlstate(),
                crate::optimizer::OptimizerError::JoinGraph(_) => None,
            }),
            state,
            "{sql}"
        );
        if let Some(expected) = expected {
            let plan = result.unwrap();
            let uqa_sql::plan::UnifiedPlan::Query(query) = plan else {
                panic!("query")
            };
            let uqa_sql::plan::RelationalPlan::QueryBlock(select) = query.root else {
                panic!("select")
            };
            assert_eq!(
                uqa_execution::scalar::eval_constant_scalar(&select.projections[0].expr).unwrap(),
                expected
            );
        }
        assert_eq!(memory.used(), 0, "{sql}");
    }
}
