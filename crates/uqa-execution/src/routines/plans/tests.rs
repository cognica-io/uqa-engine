//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    plan::UnifiedPlan,
    prepared::{
        composites::CompositeInputs, dependencies::PreparedAnalysisDependencies,
        planning::PreparedPlanUpdate,
    },
};

fn definition() -> PreparedDefinition<uqa_sql::plan::SharedExecutablePlan> {
    let mut logical_plan =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 'value'").unwrap().remove(0));
    let UnifiedPlan::Query(query) = &mut logical_plan else {
        panic!("query")
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let uqa_sql::ScalarExpr::Literal(uqa_core::Value::Str(value)) = &mut block.projections[0].expr
    else {
        panic!("text")
    };
    *value = "x".repeat(4 * 1024 * 1024);
    PreparedDefinition {
        composite_inputs: CompositeInputs::default(),
        logical_plan: logical_plan.into(),
        parameter_types: Vec::new().into(),
        result_schema: None,
        effective_search_path: None.into(),
        dependencies: PreparedAnalysisDependencies::default().into(),
        dependency_snapshot: None.into(),
    }
}

fn selection(plan: uqa_sql::plan::SharedExecutablePlan) -> PreparedPlanSelection {
    PreparedPlanSelection {
        plan: plan.clone(),
        update: PreparedPlanUpdate {
            composite_inputs: CompositeInputs::default(),
            reanalyzed: None,
            generic_plan: Some(plan),
            generic_cost: Some(1.0),
            custom_cost: None,
        },
    }
}

#[test]
fn invalidation_retires_selection_without_copying_analyzed_payload() {
    let variants = RoutinePlanVariants::new(&definition());
    let before = variants.snapshot();
    let allocation = allocation_counter::measure(|| variants.invalidate());
    let after = variants.snapshot();
    assert!(before.logical_plan.ptr_eq(&after.logical_plan));
    assert!(before.source_plan.ptr_eq(&after.source_plan));
    assert!(allocation.bytes_total < 256, "{allocation:?}");
    assert!(after.plan.is_none());
}

#[test]
fn invalidated_callback_cannot_restore_an_older_executable_or_counters() {
    let variants = RoutinePlanVariants::new(&definition());
    let old: uqa_sql::plan::SharedExecutablePlan =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0)).into();
    let new: uqa_sql::plan::SharedExecutablePlan =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 2").unwrap().remove(0)).into();
    let selected = variants
        .select(|_| {
            variants.invalidate();
            variants.select(|_| Ok(selection(new.clone())))?;
            Ok(selection(old.clone()))
        })
        .unwrap();
    assert!(selected.ptr_eq(&old));
    let retained = variants.snapshot();
    assert!(retained.plan.as_ref().unwrap().ptr_eq(&new));
    assert_eq!(retained.generic_plans, 1);
    assert_eq!(retained.custom_plans, 0);
}
