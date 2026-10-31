//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

fn layer() -> AutomaticViewLayer {
    AutomaticViewLayer {
        canonical_name: "public.v".into(),
        source_name: "public.items".into(),
        source_qualifier: "items".into(),
        source_column_map: Vec::new(),
        source_include_descendants: true,
        source_schema: RowSchema::new(vec!["id".into()]),
        columns: vec![ViewColumn {
            name: "id".into(),
            collation_oid: None,
            expression: ScalarExpr::Column("id".into()),
            writable_source_column: Some("id".into()),
        }],
        predicate: None,
        subqueries: Vec::new(),
        check_option: super::super::ViewCheckOption::None,
    }
}

fn scope(source: Option<&RowSchema>) -> ExpressionScope<'_> {
    ExpressionScope {
        target_qualifier: "v",
        returning_aliases: None,
        source,
        include_excluded: false,
    }
}

#[test]
fn view_contract_validation_borrows_large_values() {
    let expression = ScalarExpr::Row(vec![
        ScalarExpr::Column("id".into()),
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
    ]);
    let columns = BTreeSet::from(["id".into()]);
    let observed = allocation_counter::measure(|| {
        validate_public_view_expression(&expression, &columns, scope(None)).unwrap();
    });
    assert!(observed.bytes_total < 4096, "{observed:?}");
}

#[test]
fn view_layer_validation_borrows_large_values() {
    let expression = ScalarExpr::Row(vec![
        ScalarExpr::Column("id".into()),
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
    ]);
    let layer = layer();
    let observed = allocation_counter::measure(|| {
        validate_view_expression(&expression, &layer, scope(None)).unwrap();
    });
    assert!(observed.bytes_total < 4096, "{observed:?}");
}

#[test]
fn view_expression_validation_preserves_first_errors_and_row_image_namespaces() {
    let layer = layer();
    let columns = BTreeSet::from(["id".into()]);
    let source = RowSchema::new(vec!["id".into(), "source_only".into()]);
    let aliases = ReturningAliases {
        old: "prior".into(),
        new: "following".into(),
        old_explicit: true,
        new_explicit: true,
    };
    for (expression, scope, code, fragment) in [
        (
            ScalarExpr::Row(vec![
                ScalarExpr::Column("absent".into()),
                ScalarExpr::Column("id".into()),
            ]),
            scope(Some(&source)),
            "42703",
            "absent",
        ),
        (
            ScalarExpr::Row(vec![
                ScalarExpr::Column("id".into()),
                ScalarExpr::Column("absent".into()),
            ]),
            scope(Some(&source)),
            "42702",
            "id",
        ),
        (
            ScalarExpr::qualified_column("prior", "absent"),
            ExpressionScope {
                returning_aliases: Some(&aliases),
                ..scope(None)
            },
            "42703",
            "prior.absent",
        ),
        (
            ScalarExpr::qualified_column("excluded", "absent"),
            ExpressionScope {
                include_excluded: true,
                ..scope(None)
            },
            "42703",
            "excluded.absent",
        ),
    ] {
        for error in [
            validate_view_expression(&expression, &layer, scope).unwrap_err(),
            validate_public_view_expression(&expression, &columns, scope).unwrap_err(),
        ] {
            assert_eq!(error.sqlstate(), Some(code));
            assert!(error.to_string().contains(fragment), "{error}");
        }
    }
    for expression in [
        ScalarExpr::Column("source_only".into()),
        ScalarExpr::qualified_column("external", "absent"),
        ScalarExpr::ScalarSubquery(99),
        ScalarExpr::Star,
    ] {
        validate_view_expression(&expression, &layer, scope(Some(&source))).unwrap();
        validate_public_view_expression(&expression, &columns, scope(Some(&source))).unwrap();
    }
}
