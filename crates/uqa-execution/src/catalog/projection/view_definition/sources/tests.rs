//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored source syntax follows independently captured `PostgreSQL` definitions.

use super::*;
use crate::catalog::{test_support::empty_catalog, RelationLookupMode, RelationNameResolution};

#[test]
fn view_source_lists_preserve_postgresql_comma_and_join_syntax() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/lateral_namespace_oracle.expected.json"
    )))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    for (input, output, pretty) in [
        ("create_nested_view", "nested_view_definition", false),
        ("create_comma_view", "comma_view_definition", false),
        ("create_comma_view", "comma_view_definition_pretty", true),
        ("create_cross_view", "cross_view_definition", false),
        ("create_cross_view", "cross_view_definition_pretty", true),
    ] {
        let input = cases.iter().find(|case| case["id"] == input).unwrap();
        let output = cases.iter().find(|case| case["id"] == output).unwrap();
        let uqa_sql::Statement::CreateView { body, .. } =
            uqa_sql::compile(input["sql"].as_str().unwrap())
                .unwrap()
                .remove(0)
        else {
            panic!("view");
        };
        let mut plan = QueryPlan::lower(*body);
        let deparser = Deparser {
            output: None,
            catalog: &catalog,
            dynamic: resolution.clone(),
            bound: resolution.clone(),
            pretty,
            wrap: 0,
            standalone: false,
            indent: true,
            routine: None,
            aliases: std::cell::OnceCell::new(),
        };
        let scope = Scope {
            column_names_visible: true,
            ..Scope::default()
        };
        assert_eq!(
            format!("{};", deparser.query(&plan, &scope, None).unwrap()),
            output["results"][0]["rows"][0][0].as_str().unwrap(),
            "{}",
            input["sql"],
        );
        if input["id"] == "create_cross_view" {
            let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut plan.root else {
                panic!("query block");
            };
            let Some(SourcePlan::Join { kind, comma, .. }) = &mut block.from else {
                panic!("explicit join");
            };
            assert!(!*comma);
            assert_eq!(*kind, JoinKind::Cross);
            *kind = JoinKind::Inner;
            let legacy: QueryPlan =
                serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
            assert_eq!(
                format!("{};", deparser.query(&legacy, &scope, None).unwrap()),
                output["results"][0]["rows"][0][0].as_str().unwrap(),
                "legacy CROSS JOIN definition",
            );
        }
    }
}
