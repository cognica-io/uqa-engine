//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table ownership, rename, drop, and schema-definition compilation.

use super::*;

#[test]
fn foreign_table_ownership_and_drop_preserve_relation_lifecycle_semantics() {
    assert!(matches!(
        first("ALTER FOREIGN TABLE app.items OWNER TO CURRENT_USER"),
        Statement::AlterForeignTable(crate::ast::AlterForeignTableStmt {
            name,
            if_exists: false,
            action: crate::ast::AlterForeignTableAction::OwnerTo(owner),
        }) if name == "app.items" && owner == "CURRENT_USER"
    ));
    assert!(matches!(
        first("ALTER FOREIGN TABLE IF EXISTS app.items OWNER TO next_owner"),
        Statement::AlterForeignTable(crate::ast::AlterForeignTableStmt {
            name,
            if_exists: true,
            action: crate::ast::AlterForeignTableAction::OwnerTo(owner),
        }) if name == "app.items" && owner == "next_owner"
    ));
    assert!(matches!(
        first("ALTER FOREIGN TABLE app.items RENAME TO archived_items"),
        Statement::AlterForeignTable(crate::ast::AlterForeignTableStmt {
            name,
            if_exists: false,
            action: crate::ast::AlterForeignTableAction::RenameTo(new_name),
        }) if name == "app.items" && new_name == "archived_items"
    ));
    let Statement::AlterTable(alter) = first(
        "ALTER FOREIGN TABLE app.items DISABLE TRIGGER audit, ENABLE ALWAYS TRIGGER normalize",
    ) else {
        panic!("expected ALTER FOREIGN TABLE trigger actions");
    };
    assert!(matches!(
        alter.actions.as_slice(),
        [
            AlterTableAction::SetTriggerEnableMode {
                name: Some(disabled),
                mode: crate::ast::EventEnableMode::Disabled,
                ..
            },
            AlterTableAction::SetTriggerEnableMode {
                name: Some(always),
                mode: crate::ast::EventEnableMode::Always,
                ..
            }
        ] if disabled == "audit" && always == "normalize"
    ));
    let Statement::Drop(drop) =
        first("DROP FOREIGN TABLE IF EXISTS app.items, archive.items CASCADE")
    else {
        panic!("expected DROP FOREIGN TABLE");
    };
    assert_eq!(drop.kind, crate::ast::DropKind::ForeignTable);
    assert_eq!(drop.names, ["app.items", "archive.items"]);
    assert!(drop.if_exists);
    assert!(drop.cascade);
}

#[test]
fn foreign_table_compilation_preserves_schema_expressions_and_rejects_keys() {
    let Statement::CreateForeignTable(table) = first(
        "CREATE FOREIGN TABLE app.items (id integer NOT NULL DEFAULT bump(1) CHECK (bump(id) > 0), source integer, derived integer GENERATED ALWAYS AS (bump(source)) STORED, CONSTRAINT source_check CHECK (bump(source) > 0)) SERVER analytics",
    ) else {
        panic!("expected CREATE FOREIGN TABLE");
    };
    assert_eq!(table.name, "app.items");
    assert_eq!(table.columns.len(), 3);
    assert!(table.columns[0].not_null);
    assert!(table.columns[0].default.is_some());
    assert!(table.columns[0].check.is_some());
    assert!(table.columns[2].generated.is_some());
    assert_eq!(table.checks.len(), 1);
    assert_eq!(table.checks[0].name.as_deref(), Some("source_check"));

    for (sql, message) in [
        (
            "CREATE FOREIGN TABLE keyed (id integer PRIMARY KEY) SERVER analytics",
            "primary key constraints are not supported on foreign tables",
        ),
        (
            "CREATE FOREIGN TABLE keyed (id integer UNIQUE) SERVER analytics",
            "unique constraints are not supported on foreign tables",
        ),
        (
            "CREATE FOREIGN TABLE keyed (id integer REFERENCES parent(id)) SERVER analytics",
            "foreign key constraints are not supported on foreign tables",
        ),
    ] {
        let error = compile(sql).expect_err("foreign-table key must be rejected");
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert!(error.to_string().contains(message));
    }
}
