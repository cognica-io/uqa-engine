//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation names survive empty descriptors and preserve lexical scope.

use super::*;
use crate::{ast::CompositeTypeReference, schema::ScalarTypeSchema, ScalarExpr};

fn row_type(name: &str, oid: u32) -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: name.into(),
        oid,
        array_oid: oid + 1,
        relation_oid: oid + 2,
    })
}

fn source(name: &str, oid: u32, columns: &[&str]) -> RowSchema {
    RowSchema::with_qualified_types(
        name,
        columns.iter().map(|column| (*column).into()).collect(),
        vec![Some(ColumnType::Integer); columns.len()],
    )
    .with_relation_row_type(name, row_type(name, oid))
    .unwrap()
}

#[test]
fn named_whole_rows_preserve_empty_scopes_and_ordinary_column_precedence() {
    for columns in [vec![], vec!["n"], vec!["t"]] {
        let schema = source("t", 20_000, &columns);
        assert!(schema.has_qualifier("t"));
        assert_eq!(schema.len(), columns.len());
        assert_eq!(schema.physical_width(), columns.len());
        assert_eq!(schema.whole_row_type("t"), Some(&row_type("t", 20_000)));
        for expression in [
            ScalarExpr::Column("t".into()),
            ScalarExpr::QualifiedStar("t".into()),
        ] {
            let expected = if columns == ["t"] && matches!(expression, ScalarExpr::Column(_)) {
                ColumnType::Integer
            } else {
                row_type("t", 20_000)
            };
            assert_eq!(
                crate::scalar_type(&expression, &schema, &[]).unwrap(),
                Some(expected)
            );
        }
    }
    assert!(RowSchema::default()
        .with_relation_row_type("t", ColumnType::Record)
        .is_err());
    assert!(RowSchema::new(vec!["n".into()])
        .with_relation_row_type("t", row_type("t", 20_000))
        .is_err());
}

#[test]
fn relation_row_types_follow_aliases_and_scope_boundaries() {
    let schema = source("t", 20_000, &["n"]);
    let alias = RowSchema::with_relation_qualifier(&schema, "renamed");
    assert_eq!(alias.whole_row_type("renamed"), schema.whole_row_type("t"));
    assert!(!alias.has_qualifier("t"));
    let projection = RowSchema::select(&schema, &[("n".into(), "n".into())]);
    assert!(!projection.has_qualifier("t"));
    assert_eq!(projection.whole_row_type("t"), None);
    let hidden = RowSchema::append_hidden_typed(&schema, &[Some(ColumnType::Text)]);
    assert!(Arc::ptr_eq(
        schema.index.cold.row_types.0.as_ref().unwrap(),
        hidden.index.cold.row_types.0.as_ref().unwrap()
    ));
    let (canonical, _) = hidden.canonical_projection();
    assert_eq!(canonical.whole_row_type("t"), schema.whole_row_type("t"));
    let extended = RowSchema::append_typed(&schema, &[("extra".into(), Some(ColumnType::Text))]);
    let alias = RowSchema::with_relation_qualifier(&extended, "extended");
    assert_eq!(alias.whole_row_type("extended"), None);
}

#[test]
fn named_empty_relations_shadow_outer_relation_types_and_qualified_fields() {
    let outer = source("t", 20_000, &["n"]);
    let inner = source("t", 30_000, &[]);
    let scope = RowSchema::with_outer_schema(&inner, &outer);
    assert_eq!(scope.whole_row_type("t"), inner.whole_row_type("t"));
    assert!(!scope.has_qualified_column("t", "n"));
    let scope = RowSchema::with_outer_schema(&RowSchema::default(), &outer);
    assert_eq!(scope.whole_row_type("t"), outer.whole_row_type("t"));
    assert!(scope.has_qualified_column("t", "n"));
    let alias = RowSchema::with_relation_qualifier(&scope, "local");
    assert_eq!(alias.whole_row_type("local"), None);
}

#[test]
fn joins_keep_independent_relation_types_without_inventing_a_combined_named_row() {
    let left = source("a", 20_000, &[]);
    let two_empty_sources = left
        .clone()
        .with_relation_row_type("b", row_type("b", 30_000))
        .unwrap();
    assert_eq!(
        two_empty_sources.whole_row_type("a"),
        left.whole_row_type("a")
    );
    assert_eq!(
        two_empty_sources.whole_row_type("b"),
        Some(&row_type("b", 30_000))
    );
    let alias = RowSchema::with_relation_qualifier(&two_empty_sources, "combined");
    assert_eq!(alias.whole_row_type("combined"), None);
    let right = source("b", 30_000, &["n"]);
    let joined = RowSchema::join(&left, &right, Vec::new());
    assert_eq!(joined.whole_row_type("a"), left.whole_row_type("a"));
    assert_eq!(joined.whole_row_type("b"), right.whole_row_type("b"));
    let alias = RowSchema::with_relation_qualifier(&joined, "joined");
    assert_eq!(alias.whole_row_type("joined"), None);
    assert_eq!(
        crate::scalar_type(&ScalarExpr::QualifiedStar("joined".into()), &alias, &[]).unwrap(),
        Some(ColumnType::Record)
    );
    for anonymous in [
        RowSchema::default(),
        RowSchema::with_qualified_types(
            "anonymous",
            vec!["extra".into()],
            vec![Some(ColumnType::Text)],
        ),
    ] {
        let joined = RowSchema::join(&right, &anonymous, Vec::new());
        let alias = RowSchema::with_relation_qualifier(&joined, "joined");
        assert_eq!(alias.whole_row_type("joined"), None);
    }
}
