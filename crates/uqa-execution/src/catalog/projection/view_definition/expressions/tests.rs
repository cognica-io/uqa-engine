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
fn retained_literals_keep_the_original_sql_rendering() {
    use super::super::{Deparser, Scope};
    use uqa_core::Value;
    use uqa_sql::{
        ast::ColumnType, ir::ConstantValueStorage, ir::RetainedScalarLiteral, ScalarExpr,
    };

    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    let storage = crate::scalar::RetainedConstants::new(64);
    for declaration in [None, Some("\"pg_catalog\".\"text\"".to_owned())] {
        let retained = RetainedScalarLiteral {
            value: storage
                .retain(Value::Str("O'Reilly λ".repeat(128)))
                .unwrap(),
            ty: ColumnType::Text,
            declaration,
            collation_oid: None,
        };
        let original = retained.materialize().unwrap();
        let expression = ScalarExpr::RetainedLiteral(Box::new(retained));
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
            assert_eq!(
                deparser
                    .expression(&expression, &Scope::default(), &[])
                    .unwrap(),
                deparser
                    .expression(&original, &Scope::default(), &[])
                    .unwrap()
            );
        }
    }
}

#[test]
fn native_operator_index_keys_preserve_postgresql_parentheses() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("prefix_indexes.json")).unwrap();
    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    for case in reference["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let uqa_sql::Statement::CreateIndex(index) =
            uqa_sql::compile(&format!("CREATE INDEX probe ON items (({input}))"))
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        let uqa_sql::ast::IndexKey::Expression(expression) = &index.columns[0] else {
            unreachable!()
        };
        let mut call = false;
        let allocations = allocation_counter::measure(|| {
            call = super::super::stored_expression_prints_as_call(expression);
        });
        assert_eq!(
            call,
            !case["output"][0].as_str().unwrap().starts_with('('),
            "{input}"
        );
        assert_eq!(allocations.count_total, 0, "classification borrows {input}");
        for (position, pretty) in [false, true].into_iter().enumerate() {
            assert_eq!(
                index_key_definition(None, &catalog, &resolution, &index.columns[0], pretty)
                    .unwrap(),
                case["output"][position].as_str().unwrap(),
                "{input}, pretty={pretty}"
            );
        }
    }
}

#[test]
fn generated_distinct_expressions_keep_postgresql_boolean_syntax() {
    use super::super::{Deparser, Scope};
    use uqa_sql::ast::{BinaryOp, FunctionBinding, FunctionDispatch, FunctionOrderSyntax};
    use uqa_sql::ScalarExpr;

    fn call(dispatch: Option<FunctionDispatch>, args: Vec<ScalarExpr>) -> ScalarExpr {
        let binding = dispatch.map(FunctionBinding::dispatched);
        ScalarExpr::Func {
            name: binding
                .as_ref()
                .map_or_else(|| "IS DISTINCT FROM".into(), |binding| binding.name.clone()),
            binding,
            args,
            distinct: false,
            order_syntax: FunctionOrderSyntax::Ordinary,
            order_by: Vec::new(),
            filter: None,
        }
    }

    let column = |name: &str| ScalarExpr::Column(name.into());
    let first = call(
        Some(FunctionDispatch::ValueIsDistinct),
        vec![column("a"), column("b")],
    );
    let second = call(
        Some(FunctionDispatch::ValueIsDistinct),
        vec![column("c"), column("d")],
    );
    let row = call(
        Some(FunctionDispatch::RowDistinct),
        vec![first.clone(), second],
    );
    let negated = call(Some(FunctionDispatch::DistinctNot), vec![first]);
    let cases = [
        (
            row.clone(),
            "((a IS DISTINCT FROM b) OR (c IS DISTINCT FROM d))",
            "a IS DISTINCT FROM b OR c IS DISTINCT FROM d",
        ),
        (
            call(Some(FunctionDispatch::DistinctNot), vec![row.clone()]),
            "(NOT ((a IS DISTINCT FROM b) OR (c IS DISTINCT FROM d)))",
            "NOT (a IS DISTINCT FROM b OR c IS DISTINCT FROM d)",
        ),
        (
            ScalarExpr::And(vec![row, column("flag")]),
            "(((a IS DISTINCT FROM b) OR (c IS DISTINCT FROM d)) AND flag)",
            "(a IS DISTINCT FROM b OR c IS DISTINCT FROM d) AND flag",
        ),
        (
            negated.clone(),
            "(NOT (a IS DISTINCT FROM b))",
            "NOT a IS DISTINCT FROM b",
        ),
        (
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(negated),
                rhs: Box::new(column("flag")),
            },
            "((NOT (a IS DISTINCT FROM b)) = flag)",
            "(NOT a IS DISTINCT FROM b) = flag",
        ),
        (
            call(None, vec![column("a"), column("b")]),
            "\"IS DISTINCT FROM\"(a, b)",
            "\"IS DISTINCT FROM\"(a, b)",
        ),
    ];
    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    let scope = Scope {
        column_names_visible: true,
        ..Scope::default()
    };
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
        for (expression, compact, readable) in &cases {
            assert_eq!(
                deparser.expression(expression, &scope, &[]).unwrap(),
                if pretty { *readable } else { *compact },
                "pretty={pretty}, {expression:?}"
            );
        }
    }
}

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
