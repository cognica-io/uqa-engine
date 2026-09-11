//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::bind_stored_view_relations;
use crate::plan::{QueryPlan, RelationalPlan, SourcePlan};
use uqa_core::RelationIdentity;

fn lower_query(sql: &str) -> QueryPlan {
    let statement = crate::compile(sql).unwrap().remove(0);
    let crate::plan::UnifiedPlan::Query(plan) = crate::plan::UnifiedPlan::lower(statement) else {
        panic!("expected a query plan");
    };
    *plan
}

fn root_table_name(plan: &QueryPlan) -> &str {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        panic!("expected a query block");
    };
    let Some(SourcePlan::Table { name, .. }) = block.from.as_ref() else {
        panic!("expected a table source");
    };
    name
}

fn remove_plan_field(value: &mut serde_json::Value, field: &str) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                remove_plan_field(value, field);
            }
        }
        serde_json::Value::Object(fields) => {
            fields.remove(field);
            for value in fields.values_mut() {
                remove_plan_field(value, field);
            }
        }
        _ => {}
    }
}

#[test]
fn legacy_view_source_binding_requires_one_catalog_identity() {
    let mut unique = lower_query("SELECT * FROM items");
    bind_stored_view_relations(
        &mut unique,
        &std::collections::BTreeSet::from([RelationIdentity::new("app", "items")]),
    )
    .unwrap();
    assert_eq!(root_table_name(&unique), "app.items");

    let mut ambiguous = lower_query("SELECT * FROM items");
    let error = bind_stored_view_relations(
        &mut ambiguous,
        &std::collections::BTreeSet::from([
            RelationIdentity::new("app", "items"),
            RelationIdentity::new("public", "items"),
        ]),
    )
    .unwrap_err();
    assert!(error.to_string().contains("ambiguous stored view source"));

    let mut missing = lower_query("SELECT * FROM items");
    let error =
        bind_stored_view_relations(&mut missing, &std::collections::BTreeSet::new()).unwrap_err();
    assert!(error.to_string().contains("does not exist"));
}

#[test]
fn legacy_view_plans_restore_structured_source_qualifiers() {
    let mut table_json =
        serde_json::to_value(lower_query("SELECT * FROM \"app.dot\".\"items.dot\"")).unwrap();
    remove_plan_field(&mut table_json, "qualifier");
    let mut table_plan: QueryPlan = serde_json::from_value(table_json).unwrap();
    bind_stored_view_relations(
        &mut table_plan,
        &std::collections::BTreeSet::from([RelationIdentity::new("app.dot", "items.dot")]),
    )
    .unwrap();
    let RelationalPlan::QueryBlock(table_block) = &table_plan.root else {
        panic!("expected a query block");
    };
    let Some(SourcePlan::Table { qualifier, .. }) = table_block.from.as_ref() else {
        panic!("expected a table source");
    };
    assert_eq!(qualifier, "items.dot");

    let mut function_json =
        serde_json::to_value(lower_query("SELECT * FROM application.rows_for(1)")).unwrap();
    remove_plan_field(&mut function_json, "output_name");
    let mut function_plan: QueryPlan = serde_json::from_value(function_json).unwrap();
    bind_stored_view_relations(&mut function_plan, &std::collections::BTreeSet::new()).unwrap();
    let RelationalPlan::QueryBlock(function_block) = &function_plan.root else {
        panic!("expected a query block");
    };
    let Some(SourcePlan::Function { output_name, .. }) = function_block.from.as_ref() else {
        panic!("expected a function source");
    };
    assert_eq!(output_name, "rows_for");
}

#[test]
fn stored_view_binding_preserves_cte_sources() {
    let mut plan = lower_query("WITH items AS (VALUES (1)) SELECT * FROM items");
    bind_stored_view_relations(&mut plan, &std::collections::BTreeSet::new()).unwrap();
    assert_eq!(root_table_name(&plan), "items");
}

#[test]
fn restored_view_virtual_relations_and_explicit_user_sources_remain_distinct() {
    let mut virtual_query = lower_query("SELECT * FROM pg_class");
    bind_stored_view_relations(&mut virtual_query, &std::collections::BTreeSet::new()).unwrap();
    assert_eq!(root_table_name(&virtual_query), "pg_catalog.pg_class");
    let mut user_query = lower_query("SELECT * FROM public.pg_class");
    bind_stored_view_relations(
        &mut user_query,
        &std::collections::BTreeSet::from([RelationIdentity::new("public", "pg_class")]),
    )
    .unwrap();
    assert_eq!(root_table_name(&user_query), "public.pg_class");
}

#[test]
fn missing_qualified_view_sources_do_not_fall_back_to_another_schema() {
    let mut query = lower_query("SELECT * FROM app.items");
    let error = bind_stored_view_relations(
        &mut query,
        &std::collections::BTreeSet::from([RelationIdentity::new("public", "items")]),
    )
    .unwrap_err();
    assert!(error.contains("stored view source relation `app.items` does not exist"));
}
