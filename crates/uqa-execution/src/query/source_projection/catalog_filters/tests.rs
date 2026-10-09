//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn column() -> ScalarExpr {
    ScalarExpr::QualifiedColumn {
        qualifier: "c".into(),
        column: "relation".into(),
    }
}

fn equal(lhs: ScalarExpr, rhs: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn literal() -> ScalarExpr {
    ScalarExpr::Literal(Value::Str("wanted".into()))
}

fn bounded(expression: ScalarExpr, aliases: &[String]) -> CatalogRequest {
    let mut request = CatalogRequest::default();
    add_name_bounds(
        &mut request,
        &["table_name".into(), "column_name".into()],
        aliases,
        "c",
        &expression,
    );
    request
}

#[test]
fn identifier_bounds_follow_aliases_conjunctions_and_commuted_equalities() {
    let aliases = ["relation".into(), "attribute".into()];
    for predicate in [
        equal(column(), literal()),
        equal(literal(), column()),
        ScalarExpr::And(vec![equal(column(), literal())]),
        equal(
            ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(column()),
                ty: "text".into(),
            },
            literal(),
        ),
        equal(
            column(),
            ScalarExpr::TypedLiteral {
                value: Value::Str("wanted".into()),
                ty: "name".into(),
                composite_source: None,
                bound_type: Some(ColumnType::Name),
                parameter_index: Some(0),
            },
        ),
    ] {
        let request = bounded(predicate, &aliases);
        assert!(request.matches_name("table_name", "wanted"));
        assert!(!request.matches_name("table_name", "other"));
        assert!(request.matches_name("column_name", "other"));
    }
}

#[test]
fn unsupported_or_ambiguous_predicates_cannot_remove_catalog_rows() {
    let aliases = ["relation".into(), "attribute".into()];
    for predicate in [
        ScalarExpr::Or(vec![
            equal(column(), literal()),
            equal(column(), ScalarExpr::Literal(Value::Str("other".into()))),
        ]),
        ScalarExpr::Not(Box::new(equal(column(), literal()))),
        equal(column(), ScalarExpr::Param(0)),
        equal(column(), ScalarExpr::Literal(Value::Null)),
        equal(column(), ScalarExpr::Column("other".into())),
        equal(
            ScalarExpr::QualifiedColumn {
                qualifier: "other".into(),
                column: "relation".into(),
            },
            literal(),
        ),
        equal(
            ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(column()),
                ty: "text".into(),
            },
            literal(),
        ),
        equal(
            column(),
            ScalarExpr::TypedLiteral {
                value: Value::Str("wanted".into()),
                ty: "character(6)".into(),
                composite_source: None,
                bound_type: None,
                parameter_index: None,
            },
        ),
    ] {
        let request = bounded(predicate, &aliases);
        assert!(request.matches_name("table_name", "other"));
    }
    let ambiguous = bounded(
        equal(column(), literal()),
        &["relation".into(), "relation".into()],
    );
    assert!(ambiguous.matches_name("table_name", "other"));
    assert!(ambiguous.matches_name("column_name", "other"));
}
