//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::{
    projection::helpers::index_definitions::index_key_definition, test_support::empty_catalog,
    RelationLookupMode, RelationNameResolution,
};

#[test]
fn named_call_reconstruction_preserves_notation_and_keyword_quoting() {
    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    for (input, expected) in [
        (
            "array_sort(descending => true, \"array\" => ARRAY[2,1])",
            "array_sort(descending => true, \"array\" => ARRAY[2, 1])",
        ),
        (
            "array_sort(value => ARRAY[2,1])",
            "array_sort(value => ARRAY[2, 1])",
        ),
    ] {
        let uqa_sql::Statement::CreateIndex(index) =
            uqa_sql::compile(&format!("CREATE INDEX probe ON items (({input}))"))
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        for pretty in [false, true] {
            assert_eq!(
                index_key_definition(None, &catalog, &resolution, &index.columns[0], pretty)
                    .unwrap(),
                expected,
            );
        }
    }
}

#[test]
fn retained_collation_metadata_preserves_projection_column_names() {
    use super::super::{Deparser, Scope};
    use uqa_sql::ir::{ResultCollation, ScalarExpr};
    use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    let collation: ResultCollation = serde_json::from_value(serde_json::json!({
        "derivation": {"Implicit": 100}, "initial_oid": 100,
    }))
    .unwrap();
    for pretty in [false, true] {
        let deparser = Deparser {
            output: None,
            catalog: &catalog,
            dynamic: resolution.clone(),
            bound: resolution.clone(),
            pretty,
            wrap: 0,
            standalone: false,
            indent: false,
            routine: None,
            aliases: std::cell::OnceCell::new(),
        };
        let scope = Scope {
            column_names_visible: true,
            ..Scope::default()
        };
        for (input, expected) in [
            ("s", "s"),
            ("s AS s", "s"),
            ("s AS renamed", "s AS renamed"),
            ("q.s AS s", "q.s"),
            ("q.s AS renamed", "q.s AS renamed"),
            ("lower(s) AS s", "lower(s) AS s"),
            (
                "s COLLATE \"C\" AS s",
                if pretty {
                    "s COLLATE \"C\" AS s"
                } else {
                    "(s COLLATE \"C\") AS s"
                },
            ),
        ] {
            let statement = uqa_sql::compile(&format!("SELECT {input}"))
                .unwrap()
                .remove(0);
            let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
                unreachable!()
            };
            let RelationalPlan::QueryBlock(mut block) = query.root else {
                unreachable!()
            };
            for depth in 0..3 {
                let mut rendered = String::new();
                deparser
                    .target_list(&mut rendered, &block.projections, None, &scope, &[])
                    .unwrap();
                assert_eq!(
                    rendered,
                    format!(" {expected}"),
                    "{input}: pretty={pretty}, metadata depth={depth}"
                );
                let expression = &mut block.projections[0].expr;
                *expression = ScalarExpr::Collated {
                    expr: Box::new(expression.clone()),
                    ty: uqa_sql::ColumnType::Text,
                    collation,
                };
            }
        }
    }
}
