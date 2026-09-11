//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{choose_custom_plan, specialize_parameters, PreparedPlanUsage};
use uqa_core::Value;
use uqa_sql::{ColumnType, SQLParam, ScalarExpr};

#[test]
fn custom_plan_selection_respects_parameterless_queries_sampling_and_cost_ties() {
    let usage = PreparedPlanUsage {
        has_parameters: true,
        custom_plans: 4,
        total_custom_cost: 40.0,
    };
    assert!(choose_custom_plan(usage, "auto", Some(0.0)));
    assert!(!choose_custom_plan(
        usage,
        "force_generic_plan",
        Some(100.0)
    ));
    assert!(choose_custom_plan(usage, "force_custom_plan", None));
    let sampled = PreparedPlanUsage {
        custom_plans: 5,
        total_custom_cost: 50.0,
        ..usage
    };
    assert!(!choose_custom_plan(sampled, "auto", None));
    assert!(!choose_custom_plan(sampled, "auto", Some(9.0)));
    assert!(choose_custom_plan(sampled, "auto", Some(10.0)));
    for mode in ["auto", "force_generic_plan", "force_custom_plan"] {
        assert!(!choose_custom_plan(
            PreparedPlanUsage {
                has_parameters: false,
                ..usage
            },
            mode,
            Some(100.0)
        ));
    }
}

#[test]
fn specialization_preserves_bound_domain_and_parameter_provenance() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "positive".into(),
        oid: 90_001,
        base: Box::new(ColumnType::Integer),
    };
    let mut plan =
        crate::UnifiedPlan::lower(uqa_sql::compile("SELECT $1, $2, $3, $4").unwrap().remove(0));
    specialize_parameters(
        &mut plan,
        &[
            SQLParam::typed_scalar(Value::Int(7), domain.clone()),
            SQLParam::Scalar(Value::Null),
            SQLParam::Vector(vec![1.0, 2.0]),
        ],
    );
    let mut expressions = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| expressions.push(expression.clone()));
    assert!(expressions.iter().any(|expression| matches!(expression, ScalarExpr::TypedLiteral { value: Value::Int(7), bound_type: Some(ty), parameter_index: Some(1), .. } if ty == &domain)));
    assert!(expressions
        .iter()
        .any(|expression| matches!(expression, ScalarExpr::Literal(Value::Null))));
    assert!(expressions
        .iter()
        .any(|expression| matches!(expression, ScalarExpr::Param(3))));
    assert!(expressions
        .iter()
        .any(|expression| matches!(expression, ScalarExpr::Param(4))));
    assert!(!expressions
        .iter()
        .any(|expression| matches!(expression, ScalarExpr::Param(1 | 2))));
}
