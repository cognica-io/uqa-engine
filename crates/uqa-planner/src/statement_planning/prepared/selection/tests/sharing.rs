//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn cached_inputs(payload_bytes: usize) -> Inputs {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    let mut plan = UnifiedPlan::lower(
        uqa_sql::compile("SELECT $1::integer AS value, 'payload'::text AS payload")
            .unwrap()
            .remove(0),
    );
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let ScalarExpr::Literal(Value::Str(value)) = expression {
            *value = "x".repeat(payload_bytes);
        }
    });
    {
        let mut entry = inputs.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        entry.logical_plan = plan.clone().into();
        entry.source_plan = entry.logical_plan.clone();
        entry.plan = Some(plan.into());
        entry.generic_cost = Some(1.0);
        entry.result_schema = Some(RowSchema::with_types(
            vec!["value".into(), "payload".into()],
            vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
        ));
    }
    inputs
}

#[test]
fn cached_prepared_selection_does_not_copy_executable_payloads() {
    let inputs = cached_inputs(4 * 1024 * 1024);
    let cached = inputs
        .entry
        .borrow()
        .as_ref()
        .unwrap()
        .plan
        .clone()
        .unwrap();
    for used in 1..=32 {
        let mut selected = None;
        let measured = allocation_counter::measure(|| {
            selected = inputs.select().unwrap();
        });
        assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
        assert!(measured.bytes_current < 16 * 1024, "{measured:?}");
        assert!(selected.unwrap().ptr_eq(&cached));
        let snapshot = inputs.entry.borrow().as_ref().unwrap().clone();
        assert!(snapshot.plan.as_ref().unwrap().ptr_eq(&cached));
        assert_eq!(snapshot.generic_plans, used);
        assert_eq!(snapshot.custom_plans, 5);
    }
    assert!(!inputs.events.borrow().contains(&"optimize.generic"));
}

#[test]
fn generic_execution_outlives_its_invalidated_cache_without_copying() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    let selected = inputs.select().unwrap().unwrap();
    {
        let mut entry = inputs.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        assert!(entry.plan.as_ref().unwrap().ptr_eq(&selected));
        entry.invalidate();
        assert!(entry.plan.is_none());
        assert!(entry.needs_analysis);
    }
    assert!(has_parameter(&selected));
    assert_eq!(selected.strong_count(), 1);
    let released = allocation_counter::measure(|| drop(selected));
    assert!(released.bytes_current < 0, "{released:?}");

    let rebuilt = inputs.select().unwrap().unwrap();
    assert!(inputs.entry.borrow_mut().take().is_some());
    assert!(has_parameter(&rebuilt));
    assert_eq!(rebuilt.strong_count(), 1);
    let released = allocation_counter::measure(|| drop(rebuilt));
    assert!(released.bytes_current < 0, "{released:?}");
}

#[test]
fn custom_selection_does_not_modify_the_shared_generic_plan() {
    let mut inputs = cached_inputs(32);
    inputs.mode = "force_custom_plan";
    let generic = inputs
        .entry
        .borrow()
        .as_ref()
        .unwrap()
        .plan
        .clone()
        .unwrap();
    let original = serde_json::to_value(generic.as_ref()).unwrap();
    let custom = inputs.select().unwrap().unwrap();
    assert!(!generic.ptr_eq(&custom));
    assert!(!has_parameter(&custom));
    assert_eq!(serde_json::to_value(generic.as_ref()).unwrap(), original);
    let entry = inputs.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert!(entry.plan.as_ref().unwrap().ptr_eq(&generic));
    assert_eq!(entry.generic_plans, 0);
    assert_eq!(entry.custom_plans, 6);
}

#[test]
fn failed_or_unwound_selection_preserves_shared_cache_ownership() {
    struct Failure(bool);
    impl PreparedPlanOptimization for Failure {
        fn optimize_plan(
            &self,
            _: MutableExecutablePlan,
            _: &ProductionControl<'_>,
        ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
            assert!(!self.0, "optimizer unwind");
            Err(SQLError::Internal("optimizer error".into()))
        }
        fn estimate_plan(
            &self,
            _: &UnifiedPlan,
            _: &ProductionControl<'_>,
        ) -> Result<crate::plan_cost::PlanCost, SQLError> {
            panic!("failed optimization does not estimate a cost");
        }
    }
    for unwind in [false, true] {
        let mut inputs = cached_inputs(32);
        inputs.mode = "force_custom_plan";
        let generic = inputs
            .entry
            .borrow()
            .as_ref()
            .unwrap()
            .plan
            .clone()
            .unwrap();
        let original = serde_json::to_value(generic.as_ref()).unwrap();
        let failure = Failure(unwind);
        let mut context = inputs.context();
        context.optimization = &failure;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            select_plan(&context, "saved", &[SQLParam::Scalar(Value::Int(7))])
        }));
        if unwind {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result.unwrap().unwrap_err().to_string(),
                "internal error: optimizer error"
            );
        }
        let entry = inputs.entry.borrow();
        let entry = entry.as_ref().unwrap();
        assert!(entry.plan.as_ref().unwrap().ptr_eq(&generic));
        assert_eq!(generic.strong_count(), 2);
        assert_eq!(serde_json::to_value(generic.as_ref()).unwrap(), original);
        assert_eq!((entry.generic_plans, entry.custom_plans), (0, 5));
        assert!(!inputs.events.borrow().contains(&"publish"));
    }
}
