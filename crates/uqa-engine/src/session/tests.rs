//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, QueryPlan};

fn lower_query(sql: &str) -> QueryPlan {
    let statement = uqa_sql::compile(sql).unwrap().remove(0);
    let uqa_planner::UnifiedPlan::Query(plan) = uqa_planner::UnifiedPlan::lower(statement) else {
        panic!("expected a query plan");
    };
    *plan
}

#[test]
fn analyze_counter_conversion_rejects_values_above_sqlite_range() {
    let max_i64 = u64::try_from(i64::MAX).unwrap();
    assert_eq!(Engine::u64_to_i64("row count", max_i64).unwrap(), i64::MAX);
    let error = Engine::u64_to_i64("row count", max_i64 + 1).unwrap_err();
    assert!(error
        .to_string()
        .contains("exceeds the persistent i64 range"));
}

#[test]
fn legacy_view_sequence_binding_requires_one_catalog_identity() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA app", &[]).unwrap();
    engine.sql("CREATE SEQUENCE app.ids", &[]).unwrap();

    let mut unique = lower_query("SELECT nextval('ids')");
    engine
        .bind_stored_view_plan(&mut unique, &std::collections::BTreeSet::new())
        .unwrap();
    let mut references = Vec::new();
    unique.rewrite_scalar_expressions(&mut |expression| {
        if let Some(reference) = super::sequence_function_reference_mut(expression) {
            references.push(reference.clone());
        }
    });
    assert_eq!(references, ["app.ids"]);

    engine.sql("CREATE SEQUENCE public.ids", &[]).unwrap();
    let mut ambiguous = lower_query("SELECT currval('ids')");
    let error = engine
        .bind_stored_view_plan(&mut ambiguous, &std::collections::BTreeSet::new())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("ambiguous persisted sequence reference"));

    let missing_engine = Engine::new();
    let mut missing = lower_query("SELECT setval('ids', 1)");
    let error = missing_engine
        .bind_stored_view_plan(&mut missing, &std::collections::BTreeSet::new())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("dangling persisted sequence reference"));
}
