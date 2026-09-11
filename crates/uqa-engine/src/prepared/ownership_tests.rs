//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_planner::statement_planning::prepared::selection::PreparedPlanSession;
use uqa_sql::prepared::planning::PreparedPlanUpdate;

fn register(engine: &Engine, sql: &str) -> Result<(), uqa_sql::SQLError> {
    engine.register_prepared("saved".into(), uqa_sql::compile(sql).unwrap().remove(0))
}

#[test]
fn replaced_or_deallocated_definition_never_receives_a_previous_plans_usage() {
    let engine = Engine::new();
    register(&engine, "SELECT 1").unwrap();
    let original = engine.session.prepared.read()["saved"].logical_plan.clone();
    register(&engine, "SELECT 2").unwrap();
    let update = || PreparedPlanUpdate {
        generic_plan: Some((*original).clone()),
        generic_cost: Some(42.0),
        custom_cost: None,
    };
    PreparedPlanSession::publish_usage(&engine, "saved", &original, update());
    let current = engine.session.prepared.read()["saved"].clone();
    assert!(!Arc::ptr_eq(&current.logical_plan, &original));
    assert!(current.plan.is_none());
    assert!(current.generic_cost.is_none());
    assert_eq!(current.generic_plans, 0);
    assert_eq!(current.custom_plans, 0);
    engine.deallocate_prepared(Some("saved"));
    PreparedPlanSession::publish_usage(&engine, "saved", &original, update());
    assert!(!engine.session.prepared.read().contains_key("saved"));
}

#[test]
fn failed_replacement_keeps_the_original_definition_identity_and_metadata() {
    let engine = Engine::new();
    register(&engine, "SELECT 1 AS value").unwrap();
    let original = engine.session.prepared.read()["saved"].clone();
    assert!(register(&engine, "SELECT missing FROM missing_table").is_err());
    let current = engine.session.prepared.read()["saved"].clone();
    assert!(Arc::ptr_eq(&current.logical_plan, &original.logical_plan));
    assert_eq!(current.prepared_at_micros, original.prepared_at_micros);
    assert_eq!(current.parameter_types, original.parameter_types);
    assert_eq!(current.generic_plans, original.generic_plans);
    assert_eq!(current.custom_plans, original.custom_plans);
}
