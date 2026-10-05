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
        }) if name == "app.items" && owner == crate::ast::RoleSpecification::CurrentUser
    ));
    assert!(matches!(
        first("ALTER FOREIGN TABLE IF EXISTS app.items OWNER TO next_owner"),
        Statement::AlterForeignTable(crate::ast::AlterForeignTableStmt {
            name,
            if_exists: true,
            action: crate::ast::AlterForeignTableAction::OwnerTo(owner),
        }) if name == "app.items" && owner == crate::ast::RoleSpecification::from("next_owner")
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
    let Statement::CreateForeignTableDefinition(deferred) = first(
        "CREATE FOREIGN TABLE app.items (id integer NOT NULL DEFAULT bump(1) CHECK (bump(id) > 0), source integer, derived integer GENERATED ALWAYS AS (bump(source)) STORED, CONSTRAINT source_check CHECK (bump(source) > 0)) SERVER analytics",
    ) else {
        panic!("expected deferred CREATE FOREIGN TABLE");
    };
    let table = resolve(&deferred).unwrap();
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
        let error = resolve(&definition(sql)).expect_err("foreign-table key must be rejected");
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert!(error.to_string().contains(message));
    }
}

#[derive(Default)]
struct Types {
    reads: std::sync::Mutex<Vec<String>>,
}

impl crate::type_resolution::FunctionTypeResolver for Types {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>> {
        Ok(None)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>> {
        self.reads.lock().unwrap().push(name.to_string());
        if name == "\"fixture_type\"" {
            Ok(Some(ColumnType::Integer))
        } else {
            Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("type {name} does not exist"),
            })
        }
    }
}

fn definition(sql: &str) -> crate::ast::DeferredCreateForeignTable {
    let Statement::CreateForeignTableDefinition(definition) = first(sql) else {
        panic!("expected deferred CREATE FOREIGN TABLE");
    };
    definition
}

pub(super) fn resolve(
    definition: &crate::ast::DeferredCreateForeignTable,
) -> Result<crate::ast::CreateForeignTable> {
    resolve_deferred_create_foreign_table(definition, &Types::default())
}

#[test]
fn foreign_keys_fail_in_written_element_and_clause_order_before_ordinary_key_lowering() {
    for (columns, kind) in [
        (
            "a integer REFERENCES missing_parent(a, b), PRIMARY KEY (a)",
            "foreign key",
        ),
        ("a integer UNIQUE, PRIMARY KEY (a)", "unique"),
        (
            "a integer, FOREIGN KEY (a) REFERENCES missing_parent(a, b), PRIMARY KEY (a)",
            "foreign key",
        ),
        ("a integer, EXCLUDE USING btree (a WITH =)", "exclusion"),
        ("a integer PRIMARY KEY DEFAULT 0 DEFAULT 1", "primary key"),
        ("a integer PRIMARY KEY NULL", "primary key"),
        (
            "a integer DEFAULT missing_function(), b integer UNIQUE",
            "unique",
        ),
        (
            "a integer REFERENCES missing_parent DEFERRABLE INITIALLY DEFERRED",
            "foreign key",
        ),
    ] {
        let statement = definition(&format!(
            "CREATE FOREIGN TABLE fixture ({columns}) SERVER missing_server"
        ));
        let error = resolve(&statement).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{columns}: {error}");
        assert_eq!(
            error.to_string(),
            format!("{kind} constraints are not supported on foreign tables"),
            "{columns}"
        );
    }
}

#[test]
fn foreign_column_types_then_attributes_then_clause_conflicts_determine_the_error() {
    for (columns, state, message) in [
        (
            "a missing_type PRIMARY KEY",
            "42704",
            "type \"missing_type\" does not exist",
        ),
        (
            "a serial[] PRIMARY KEY",
            "0A000",
            "array of serial is not implemented",
        ),
        (
            "a integer NULL PRIMARY KEY",
            "42601",
            "conflicting NULL/NOT NULL declarations for column \"a\" of table \"fixture\"",
        ),
        (
            "a integer PRIMARY KEY DEFERRABLE NOT DEFERRABLE",
            "42601",
            "multiple DEFERRABLE/NOT DEFERRABLE clauses not allowed",
        ),
        (
            "a integer PRIMARY KEY, b missing_type",
            "0A000",
            "primary key constraints are not supported on foreign tables",
        ),
        (
            "a integer NOT NULL CONSTRAINT one_nn NOT NULL CONSTRAINT two_nn NOT NULL",
            "XX000",
            "conflicting not-null constraint names \"one_nn\" and \"two_nn\"",
        ),
    ] {
        let statement = definition(&format!(
            "CREATE FOREIGN TABLE fixture ({columns}) SERVER missing_server"
        ));
        let error = resolve(&statement).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{columns}: {error}");
        assert_eq!(error.to_string(), message, "{columns}");
    }
}

#[test]
fn foreign_not_null_declarations_preserve_table_order_and_named_column_clauses() {
    let statement = definition(
        "CREATE FOREIGN TABLE fixture (a integer CONSTRAINT one_nn NOT NULL NOT NULL, CONSTRAINT table_nn NOT NULL b, b integer, c serial) SERVER source",
    );
    let table = resolve(&statement).unwrap();
    let declarations = table.not_null_declarations.unwrap();
    let kept = declarations
        .iter()
        .map(|declaration| {
            (
                declaration.column.as_str(),
                declaration.name.as_deref(),
                declaration.explicit,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kept,
        [
            ("a", Some("one_nn"), true),
            ("a", None, true),
            ("b", Some("table_nn"), true),
            ("c", None, false),
        ]
    );
    let statement = definition(
        "CREATE FOREIGN TABLE fixture (a integer, NOT NULL missing, PRIMARY KEY(a)) SERVER source",
    );
    let error = resolve(&statement).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "primary key constraints are not supported on foreign tables"
    );
}

#[test]
fn resolved_foreign_column_types_are_retained_and_later_elements_are_not_read_after_error() {
    let types = Types::default();
    let statement = definition(
        "CREATE FOREIGN TABLE fixture (a fixture_type, b integer NOT NULL) SERVER missing_server",
    );
    let resolved = resolve_deferred_create_foreign_table(&statement, &types).unwrap();
    assert_eq!(resolved.columns[0].ty, ColumnType::Integer);
    assert!(resolved.columns[1].not_null);
    assert_eq!(*types.reads.lock().unwrap(), ["\"fixture_type\""]);
    types.reads.lock().unwrap().clear();
    let statement = definition(
        "CREATE FOREIGN TABLE fixture (a integer UNIQUE, b fixture_type) SERVER missing_server",
    );
    assert!(resolve_deferred_create_foreign_table(&statement, &types).is_err());
    assert!(types.reads.lock().unwrap().is_empty());
}

#[test]
fn deferred_foreign_definition_verifies_the_preflight_target_before_analysis() {
    let original =
        definition("CREATE FOREIGN TABLE IF NOT EXISTS fixture (a fixture_type) SERVER source");
    for changed in 0..3 {
        let mut statement = original.clone();
        match changed {
            0 => statement.name = "other".into(),
            1 => statement.server_name = "other".into(),
            _ => statement.if_not_exists = false,
        }
        let types = Types::default();
        let error = resolve_deferred_create_foreign_table(&statement, &types).unwrap_err();
        assert!(error.to_string().contains("changed target identity"));
        assert!(types.reads.lock().unwrap().is_empty());
    }
}

#[test]
fn foreign_definition_ast_and_plan_read_legacy_if_not_exists_without_changing_plain_creation() {
    let deferred =
        definition("CREATE FOREIGN TABLE IF NOT EXISTS fixture (a integer) SERVER source");
    let mut legacy = serde_json::to_value(&deferred).unwrap();
    legacy.as_object_mut().unwrap().remove("if_not_exists");
    let legacy = serde_json::json!({"CreateForeignTableIfNotExists": legacy});
    let Statement::CreateForeignTableDefinition(statement) =
        serde_json::from_value::<Statement>(legacy.clone()).unwrap()
    else {
        panic!("expected deferred definition");
    };
    assert!(statement.if_not_exists);
    let crate::plan::CommandPlan::CreateForeignTableDefinition(plan) =
        serde_json::from_value::<crate::plan::CommandPlan>(legacy).unwrap()
    else {
        panic!("expected deferred plan");
    };
    assert!(plan.if_not_exists);
    resolve(&statement).unwrap();
    for clause in ["", "IF NOT EXISTS "] {
        let statement = first(&format!(
            "CREATE FOREIGN TABLE {clause}fixture (a integer) SERVER source"
        ));
        let stored = serde_json::to_value(statement).unwrap();
        assert!(stored.get("CreateForeignTableDefinition").is_some());
        let Statement::CreateForeignTableDefinition(restored) =
            serde_json::from_value::<Statement>(stored).unwrap()
        else {
            panic!("expected deferred definition");
        };
        assert_eq!(restored.if_not_exists, !clause.is_empty());
        resolve(&restored).unwrap();
    }
}
