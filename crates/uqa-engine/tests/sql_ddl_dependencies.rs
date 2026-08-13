//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;
use uqa_sql::ast::{ColumnDef, ColumnType, Expr};

fn integer_column(name: &str, default: Option<Expr>) -> ColumnDef {
    ColumnDef {
        name: name.to_string(),
        ty: ColumnType::Integer,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        auto_increment: false,
        unique: false,
        default,
        check: None,
        check_name: None,
        check_enforced: true,
        references: None,
    }
}

#[test]
fn cascade_flags_and_wrong_relation_kinds_fail_before_side_effects() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA app", &[]).unwrap();
    engine
        .sql("CREATE TABLE app.items (id INTEGER, kept INTEGER)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE VIEW app.items_view AS SELECT id FROM app.items",
            &[],
        )
        .unwrap();

    for (sql, expected) in [
        ("DROP SCHEMA app CASCADE", "DROP SCHEMA CASCADE"),
        ("DROP VIEW app.items_view CASCADE", "DROP VIEW CASCADE"),
        (
            "ALTER TABLE app.items DROP COLUMN id CASCADE",
            "DROP COLUMN CASCADE",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }

    for (sql, expected) in [
        ("DROP TABLE IF EXISTS app.items_view", "not a table"),
        ("DROP VIEW IF EXISTS app.items", "not a view"),
        (
            "ALTER TABLE IF EXISTS app.items_view ADD COLUMN bad INTEGER",
            "not a table",
        ),
        ("DROP TABLE app.items, app.items_view", "not a table"),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }

    for error in [
        engine.drop_table("app.items_view").unwrap_err(),
        engine.drop_column("app.items_view", "id").unwrap_err(),
        engine
            .rename_column("app.items_view", "id", "renamed_id")
            .unwrap_err(),
        engine
            .rename_table("app.items_view", "renamed_view")
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("not a table"), "{error}");
    }

    assert!(engine.has_schema("app").unwrap());
    assert!(engine.has_table("app.items").unwrap());
    assert!(engine.view("app.items_view").unwrap().is_some());
    assert_eq!(
        engine.table_columns("app.items").unwrap(),
        vec!["id", "kept"]
    );

    engine.sql("CREATE SCHEMA routine_app", &[]).unwrap();
    engine
        .sql(
            "CREATE FUNCTION routine_app.kept() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
            &[],
        )
        .unwrap();
    let error = engine.sql("DROP SCHEMA routine_app", &[]).unwrap_err();
    assert!(error.to_string().contains("not empty"), "{error}");
    assert!(engine.has_schema("routine_app").unwrap());
}

#[test]
fn dependent_views_block_table_and_column_ddl_with_their_name() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE items (id INTEGER, kept INTEGER)", &[])
        .unwrap();
    engine
        .sql("CREATE VIEW item_ids AS SELECT id FROM items", &[])
        .unwrap();
    engine
        .sql(
            "CREATE VIEW nested_item_ids AS SELECT id FROM item_ids",
            &[],
        )
        .unwrap();

    for sql in [
        "DROP TABLE items",
        "DROP TABLE items CASCADE",
        "ALTER TABLE items RENAME TO renamed_items",
        "ALTER TABLE items RENAME COLUMN id TO item_id",
        "ALTER TABLE items DROP COLUMN id",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert!(
            error.to_string().contains("public.item_ids"),
            "{sql}: {error}"
        );
    }

    assert!(engine.has_table("items").unwrap());
    assert!(!engine.has_table("renamed_items").unwrap());
    assert_eq!(engine.table_columns("items").unwrap(), vec!["id", "kept"]);
    assert!(engine.view("item_ids").unwrap().is_some());

    let error = engine.drop_view("item_ids").unwrap_err();
    assert!(
        error.to_string().contains("public.nested_item_ids"),
        "{error}"
    );
    engine
        .sql("DROP VIEW item_ids, nested_item_ids", &[])
        .unwrap();
    assert!(engine.view("item_ids").unwrap().is_none());
    assert!(engine.view("nested_item_ids").unwrap().is_none());

    engine
        .sql("CREATE TABLE batch_parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE TABLE batch_child (parent_id INTEGER REFERENCES batch_parent(id))",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP TABLE batch_parent, batch_child", &[])
        .unwrap();
    assert!(!engine.has_table("batch_parent").unwrap());
    assert!(!engine.has_table("batch_child").unwrap());
}

#[test]
fn typed_default_dependency_is_rewritten_and_drop_is_restricted() {
    let engine = Engine::new();
    engine.create_default_table("defaults", Vec::new()).unwrap();
    engine
        .register_column("defaults", integer_column("source", None))
        .unwrap();
    engine
        .register_column(
            "defaults",
            integer_column("derived", Some(Expr::Column("source".into()))),
        )
        .unwrap();

    let error = engine.drop_column("defaults", "source").unwrap_err();
    assert!(error.to_string().contains("DEFAULT/CHECK"), "{error}");
    assert_eq!(
        engine.table_columns("defaults").unwrap(),
        vec!["source", "derived"]
    );

    assert!(engine
        .rename_column("defaults", "source", "source_new")
        .unwrap());
    assert!(matches!(
        engine
            .column_default_expr("defaults", "derived")
            .unwrap(),
        Some(Expr::Column(column)) if column == "source_new"
    ));
}

fn verify_renamed_dependencies_after_reopen(database: &std::path::Path) {
    let reopened = Engine::open(database).unwrap();
    assert!(reopened.has_table("parent_new").unwrap());
    let foreign_key = reopened.foreign_keys("child").unwrap().remove(0);
    assert_eq!(foreign_key.ref_table, "public.parent_new");
    assert_eq!(foreign_key.ref_columns, vec!["parent_key"]);
    let index = reopened.catalog_index("idx_parent_code").unwrap().unwrap();
    assert_eq!(index.table_name, "public.parent_new");
    assert_eq!(index.columns_json, "[\"code_value\"]");
    assert!(reopened
        .sql(
            "INSERT INTO parent_new (parent_key, code_value) VALUES (3, 0)",
            &[],
        )
        .is_err());

    reopened.begin().unwrap();
    reopened.sql("DROP TABLE parent_new CASCADE", &[]).unwrap();
    assert!(!reopened.has_table("parent_new").unwrap());
    assert!(reopened.has_table("child").unwrap());
    assert!(reopened.foreign_keys("child").unwrap().is_empty());
    reopened.rollback().unwrap();
    assert!(reopened.has_table("parent_new").unwrap());
    let restored_foreign_key = reopened.foreign_keys("child").unwrap().remove(0);
    assert_eq!(restored_foreign_key.ref_table, "public.parent_new");
    assert_eq!(restored_foreign_key.ref_columns, vec!["parent_key"]);

    reopened.sql("DROP TABLE parent_new CASCADE", &[]).unwrap();
    assert!(reopened.has_table("child").unwrap());
    assert!(reopened.foreign_keys("child").unwrap().is_empty());
    drop(reopened);

    let reopened = Engine::open(database).unwrap();
    assert!(!reopened.has_table("parent_new").unwrap());
    assert!(reopened.has_table("child").unwrap());
    assert!(reopened.foreign_keys("child").unwrap().is_empty());
}

#[test]
fn foreign_keys_checks_and_indexes_follow_rename_reopen_and_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("ddl-dependencies.db");
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY, code INTEGER, CHECK (code > 0))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_parent_code ON parent (code)", &[])
        .unwrap();

    let error = engine.sql("DROP TABLE parent", &[]).unwrap_err();
    assert!(error.to_string().contains("foreign key"), "{error}");

    engine.begin().unwrap();
    engine
        .sql("ALTER TABLE parent RENAME TO rolled_back_parent", &[])
        .unwrap();
    assert!(engine.has_table("rolled_back_parent").unwrap());
    engine.rollback().unwrap();
    assert!(engine.has_table("parent").unwrap());
    assert!(!engine.has_table("rolled_back_parent").unwrap());
    assert_eq!(
        engine.foreign_keys("child").unwrap()[0].ref_table,
        "public.parent"
    );

    engine.begin().unwrap();
    engine
        .sql(
            "ALTER TABLE parent RENAME COLUMN code TO rolled_back_code",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .catalog_index("idx_parent_code")
            .unwrap()
            .unwrap()
            .columns_json,
        "[\"rolled_back_code\"]"
    );
    engine.rollback().unwrap();
    assert_eq!(engine.table_columns("parent").unwrap(), vec!["id", "code"]);
    assert_eq!(
        engine
            .catalog_index("idx_parent_code")
            .unwrap()
            .unwrap()
            .columns_json,
        "[\"code\"]"
    );

    engine
        .sql("ALTER TABLE parent RENAME TO parent_new", &[])
        .unwrap();
    engine
        .sql("ALTER TABLE parent_new RENAME COLUMN id TO parent_key", &[])
        .unwrap();
    engine
        .sql(
            "ALTER TABLE parent_new RENAME COLUMN code TO code_value",
            &[],
        )
        .unwrap();

    let foreign_key = engine.foreign_keys("child").unwrap().remove(0);
    assert_eq!(foreign_key.ref_table, "public.parent_new");
    assert_eq!(foreign_key.ref_columns, vec!["parent_key"]);
    let index = engine.catalog_index("idx_parent_code").unwrap().unwrap();
    assert_eq!(index.table_name, "public.parent_new");
    assert_eq!(index.columns_json, "[\"code_value\"]");
    engine
        .sql(
            "INSERT INTO parent_new (parent_key, code_value) VALUES (1, 1)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO child (id, parent_id) VALUES (1, 1)", &[])
        .unwrap();
    assert!(engine
        .sql(
            "INSERT INTO parent_new (parent_key, code_value) VALUES (2, 0)",
            &[],
        )
        .is_err());
    assert!(engine
        .sql("INSERT INTO child (id, parent_id) VALUES (2, 999)", &[])
        .is_err());
    drop(engine);
    verify_renamed_dependencies_after_reopen(&database);
}
