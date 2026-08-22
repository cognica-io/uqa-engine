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
mod row_locking;
mod schema_definitions;
