//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnType, FromClause, JoinKind, OrderBy, Projection, TableKeyConstraintKind};

#[test]
fn bundled_parser_is_postgresql_18_4() {
    let parsed = pg_query::parse("SELECT 1").expect("parser accepts a scalar query");
    assert_eq!(parsed.protobuf.version, 180_004);
}

#[test]
fn function_arguments_reject_positional_after_named_and_duplicate_names() {
    let error = compile("SELECT random(max => 2, 1)").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(
        error.to_string(),
        "positional argument cannot follow named argument"
    );

    let error = compile("SELECT random(min => 1, min => 2)").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(
        error.to_string(),
        "argument name \"min\" used more than once"
    );
}

#[test]
fn returning_row_aliases_preserve_quoted_identifier_case() {
    let Statement::Insert(insert) = first(
        "INSERT INTO items VALUES (1) RETURNING WITH (OLD AS \"Image\", NEW AS \"image\") \"Image\".*, \"image\".*",
    ) else {
        panic!("expected INSERT");
    };
    assert_eq!(insert.returning_aliases.old, "Image");
    assert_eq!(insert.returning_aliases.new, "image");
    assert!(insert.returning_aliases.old_explicit);
    assert!(insert.returning_aliases.new_explicit);

    let error =
        compile("INSERT INTO items VALUES (1) RETURNING WITH (OLD AS image, NEW AS IMAGE) image.*")
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42712"));
    assert!(error
        .to_string()
        .contains("table name \"image\" specified more than once"));
}

#[test]
fn syntax_calls_preserve_polymorphic_builtin_identity() {
    let Statement::Select(select) = first(
        "SELECT coalesce(1, 2), greatest(1, 2), least(1, 2), nullif(1, 2),
                \"coalesce\"(1, 2), ordinary.coalesce(1, 2)",
    ) else {
        panic!("expected SELECT");
    };
    for (projection, expected_name) in select.projections[..4]
        .iter()
        .zip(["coalesce", "greatest", "least", "nullif"])
    {
        let Expr::Func {
            name,
            binding: Some(binding),
            ..
        } = &projection.expr
        else {
            panic!("expected syntax call for {expected_name}");
        };
        assert_eq!(name, expected_name);
        assert_eq!(binding.name, expected_name);
        assert!(binding.builtin);
        assert!(binding.argument_types.is_empty());
    }
    for (projection, expected_name) in select.projections[4..]
        .iter()
        .zip(["\"coalesce\"", "ordinary.coalesce"])
    {
        assert!(matches!(
            &projection.expr,
            Expr::Func {
                name,
                binding: None,
                ..
            } if name == expected_name
        ));
    }
}

fn first(sql: &str) -> Statement {
    let mut v = compile(sql).unwrap();
    assert_eq!(v.len(), 1, "expected 1 stmt");
    v.remove(0)
}

fn null_literal_node() -> Node {
    Node {
        node: Some(NodeEnum::AConst(pg_query::protobuf::AConst {
            isnull: true,
            ..Default::default()
        })),
    }
}

mod compiler_invariants;
mod data_commands;
mod ddl_lifecycle;
mod grouping;
mod query_features;
mod relations;
mod routines;
mod row_locking;
mod schema_definitions;
