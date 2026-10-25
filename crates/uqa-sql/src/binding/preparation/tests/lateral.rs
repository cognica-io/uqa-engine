//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s namespace permission errors precede argument typing and folding.

use super::*;

#[test]
fn nested_lateral_namespaces_match_postgresql_diagnostics() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/lateral_namespace_oracle.expected.json"
    ))
    .unwrap();
    let definitions = [
        "CREATE TABLE target_rows(id INTEGER PRIMARY KEY,value INTEGER)",
        "CREATE TABLE src_one(x INTEGER)",
    ];
    let tables = definitions
        .into_iter()
        .map(|sql| {
            let crate::ast::Statement::CreateTable(definition) =
                crate::compile(sql).unwrap().remove(0)
            else {
                panic!("table");
            };
            (
                crate::RelationIdentity::from_legacy_name(&format!("public.{}", definition.name))
                    .unwrap(),
                crate::binding::fixture::table_definition(definition.columns),
            )
        })
        .collect();
    let mut context = crate::binding::fixture::empty_binding_context();
    context.catalog = crate::binding::fixture::catalog(tables);
    let mut mismatches = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        if case["error"].is_null() {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let preparation =
            infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context).map(|_| ());
        let storage = match &mut plan {
            UnifiedPlan::Query(query) => crate::binding::bind_query_plan_routines_for_storage(
                &NoRoutines,
                query,
                &[],
                &context,
                None,
            )
            .map(|_| ()),
            UnifiedPlan::Command(command) => SchemaScope::for_analysis(&context)
                .unwrap()
                .bind_command_routines_for_storage(&NoRoutines, command, &[], None),
        };
        for (path, result) in [("preparation", preparation), ("storage", storage)] {
            let actual = match result {
                Ok(()) => serde_json::Value::Null,
                Err(error) => {
                    serde_json::json!({"sqlstate": error.sqlstate(), "message": error.to_string(), "detail": error.detail(), "hint": error.hint()})
                }
            };
            if actual != case["error"] {
                mismatches.push(format!(
                    "{path}: {sql}\nactual: {actual}\nexpected: {}",
                    case["error"]
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn stored_lateral_view_keeps_preceding_namespaces() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/lateral_namespace_oracle.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    let create = cases
        .iter()
        .find(|case| case["id"] == "create_nested_view")
        .unwrap();
    let rows = cases
        .iter()
        .find(|case| case["id"] == "nested_view_rows")
        .unwrap();
    let crate::ast::Statement::CreateView { body, .. } =
        crate::compile(create["sql"].as_str().unwrap())
            .unwrap()
            .remove(0)
    else {
        panic!("view");
    };
    let mut plan = QueryPlan::lower(*body);
    let context = crate::binding::fixture::empty_binding_context();
    let schema = crate::binding::bind_query_plan_routines_for_storage(
        &NoRoutines,
        &mut plan,
        &[],
        &context,
        None,
    )
    .unwrap();
    assert_eq!(
        serde_json::json!(schema.columns()),
        rows["results"][0]["columns"]
    );
}

#[test]
fn lateral_outer_join_permissions_match_postgresql_diagnostics() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
    ))
    .unwrap();
    let context = crate::binding::fixture::empty_binding_context();
    for case in reference["cases"].as_array().unwrap() {
        if !case["id"].as_str().unwrap().starts_with("forbidden_") {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context).unwrap_err();
        assert_eq!(
            serde_json::json!({
                "sqlstate": error.sqlstate(),
                "message": error.to_string(),
                "detail": error.detail(),
                "hint": error.hint(),
            }),
            case["error"],
            "{sql}"
        );
    }
}

#[test]
fn stored_and_prepared_queries_share_metadata_namespaces() {
    let context = assignment_context();
    for sql in [
        "SELECT (SELECT _meta.doc_id), (SELECT _meta.score), (SELECT tableoid), (SELECT xmin) FROM assignment_target",
        "SELECT id FROM assignment_target JOIN (VALUES (1)) marker(n) ON marker.n=(SELECT _meta.doc_id)",
        "SELECT id FROM assignment_target JOIN (VALUES (1)) marker(n) ON (SELECT tableoid)=1::oid AND (SELECT xmin)='1'::xid",
        "SELECT id FROM assignment_target JOIN (VALUES (1)) _meta(doc_id) ON _meta.doc_id=(SELECT _meta.doc_id)",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let UnifiedPlan::Query(query) = &mut plan else { panic!("query") };
        crate::binding::bind_query_plan_routines_for_storage(
            &NoRoutines, query, &[], &context, None,
        ).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let plan = UnifiedPlan::lower(crate::compile(
        "SELECT a.id FROM assignment_target a JOIN assignment_target b ON (SELECT _meta.doc_id)=1",
    ).unwrap().remove(0));
    let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P01"));
}
