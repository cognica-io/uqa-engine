//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken, Value,
};

#[test]
fn collation_observation_binding_matches_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("collation_observation.json")).unwrap();
    let context = crate::binding::fixture::empty_binding_context();
    for case in cases {
        let sql = case["sql"].as_str().unwrap();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None);
        let observed = result.and_then(|_| observe(plan));
        if case["error"].is_null() {
            assert_eq!(observed.unwrap(), case["results"][0]["rows"][0], "{sql}");
        } else {
            let error = observed.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{sql}: {error}"
            );
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap(),
                "{sql}"
            );
        }
    }
}

fn observe(plan: UnifiedPlan) -> Result<serde_json::Value, SQLError> {
    let UnifiedPlan::Query(query) = plan else {
        panic!("SELECT")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(32 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = Vec::new();
    for projection in block.projections {
        let ordinary = crate::bind_type_introspection(projection.expr.clone(), &schema, &[]);
        let controlled = crate::bind_type_introspection_with_control(
            projection.expr.clone_with_control(&control)?,
            &schema,
            &[],
            &control,
        )?;
        assert_eq!(ordinary, *controlled);
        let encoded = serde_json::to_string(&ordinary).unwrap();
        let restored: ScalarExpr = serde_json::from_str(&encoded).unwrap();
        let mut binding = None;
        restored.visit(&mut |node| {
            if let ScalarExpr::Func {
                binding: Some(selected),
                ..
            } = node
            {
                if matches!(
                    selected.dispatch,
                    Some(crate::ast::FunctionDispatch::CollationFor { .. })
                ) {
                    assert!(binding.is_none());
                    binding = Some(selected.clone());
                }
            }
        });
        let binding = binding.expect("retained collation observation");
        let context = crate::expr::EvalContext::new(None, &[]);
        let ordinary = crate::expr::eval_bound_builtin_function_call(
            &binding,
            vec![(None, Value::Null)],
            &context,
        );
        let mut arguments = ProductionVec::new(control);
        arguments
            .push_produced(control.finish((None, Value::Null), control.empty_reservation())?)?;
        let controlled = crate::expr::eval_builtin_function_call_with_control(
            &binding.name,
            Some(&binding),
            arguments.finish()?,
            &control,
            &context,
        );
        match (ordinary, controlled) {
            (Ok(value), Ok(controlled)) => {
                assert_eq!(value, *controlled);
                output.push(match value {
                    Value::Null => serde_json::Value::Null,
                    Value::Str(text) => text.into(),
                    _ => panic!("text result"),
                });
            }
            (Err(error), Err(controlled)) => {
                assert_eq!(error.sqlstate(), controlled.sqlstate());
                assert_eq!(error.to_string(), controlled.to_string());
                return Err(error);
            }
            values => panic!("ordinary/controlled mismatch: {values:?}"),
        }
    }
    assert_eq!(budget.used(), 0);
    Ok(output.into())
}
