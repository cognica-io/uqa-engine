//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterForeignTableAction, AlterViewAction, AlterViewKind};

#[test]
fn schema_moves_preserve_the_native_relation_kind_and_quoted_destination() {
    for (kind, expected) in [
        ("VIEW", AlterViewKind::View),
        ("MATERIALIZED VIEW", AlterViewKind::MaterializedView),
    ] {
        let Statement::AlterView(statement) = first(&format!(
            "ALTER {kind} IF EXISTS app.items SET SCHEMA \"New.Schema\""
        )) else {
            panic!("expected native view alteration");
        };
        assert_eq!(statement.name, "app.items");
        assert_eq!(statement.kind, expected);
        assert!(statement.if_exists);
        assert_eq!(
            statement.action,
            AlterViewAction::SetSchema("\"New.Schema\"".into())
        );
    }
    let Statement::AlterForeignTable(statement) =
        first("ALTER FOREIGN TABLE IF EXISTS app.items SET SCHEMA archive")
    else {
        panic!("expected native foreign-table alteration");
    };
    assert_eq!(statement.name, "app.items");
    assert!(statement.if_exists);
    assert_eq!(
        statement.action,
        AlterForeignTableAction::SetSchema("archive".into())
    );
    let encoded = serde_json::to_value(&statement).unwrap();
    assert_eq!(encoded["set_schema"], "archive");
    assert_eq!(
        serde_json::from_value::<crate::ast::AlterForeignTableStmt>(encoded).unwrap(),
        statement
    );
}

#[test]
fn table_and_sequence_schema_moves_keep_their_existing_lifecycle_shape() {
    let Statement::AlterTable(statement) =
        first("ALTER TABLE IF EXISTS app.items SET SCHEMA archive")
    else {
        panic!("expected ALTER TABLE");
    };
    assert_eq!(statement.table, "app.items");
    assert!(statement.if_exists);
    assert!(!statement.recurse);
    assert!(
        matches!(statement.actions.as_slice(), [AlterTableAction::SetSchema { schema }] if schema == "archive")
    );
    let Statement::AlterSequence(statement) =
        first("ALTER SEQUENCE IF EXISTS app.counter SET SCHEMA archive")
    else {
        panic!("expected ALTER SEQUENCE");
    };
    assert_eq!(statement.name, "app.counter");
    assert!(statement.if_exists);
    assert!(
        matches!(statement.lifecycle, crate::ast::SequenceLifecycle::SetSchema { schema } if schema == "archive")
    );
}
