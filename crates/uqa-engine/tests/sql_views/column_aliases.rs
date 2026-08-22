//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 view column-name list coverage.

use super::*;

#[test]
fn view_column_names_are_positional_partial_typed_and_filterable() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE source (small SMALLINT, label VARCHAR(3))",
    );
    exec(&engine, "INSERT INTO source VALUES (7, 'abc')");
    exec(
        &engine,
        "CREATE VIEW aliased (renamed_small) AS SELECT small, label FROM source",
    );

    let result = exec(
        &engine,
        "SELECT renamed_small, label FROM aliased WHERE renamed_small = 7",
    );
    assert_eq!(result.columns, ["renamed_small", "label"]);
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Varchar(Some(3)))
        ]
    );
    assert_eq!(result.rows[0]["renamed_small"], Value::Int(7));
    assert_eq!(result.rows[0]["label"], Value::Str("abc".into()));

    exec(
        &engine,
        "CREATE VIEW quoted (\"Renamed.Small\", \"Label\") AS SELECT small, label FROM source",
    );
    let quoted = exec(&engine, "SELECT \"Renamed.Small\", \"Label\" FROM quoted");
    assert_eq!(quoted.columns, ["Renamed.Small", "Label"]);
    assert_eq!(quoted.rows[0]["Renamed.Small"], Value::Int(7));

    exec(
        &engine,
        "CREATE VIEW expression_alias (value) AS SELECT 1 + 1",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM expression_alias").rows[0]["value"],
        Value::Int(2)
    );
}

#[test]
fn view_column_name_validation_matches_postgresql_18() {
    let engine = Engine::new();
    let excess = engine
        .sql("CREATE VIEW excess (a, b) AS SELECT 1", &[])
        .unwrap_err();
    assert_eq!(excess.sqlstate(), Some("42601"));

    for sql in [
        "CREATE VIEW duplicate (same, same) AS SELECT 1, 2",
        "CREATE VIEW case_duplicate (a, \"a\") AS SELECT 1, 2",
        "CREATE VIEW derived_duplicate AS SELECT 1 AS same, 2 AS same",
    ] {
        let duplicate = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(duplicate.sqlstate(), Some("42701"), "{sql}: {duplicate}");
    }

    exec(
        &engine,
        "CREATE VIEW case_distinct (a, \"A\") AS SELECT 1, 2",
    );
    exec(&engine, "CREATE VIEW system_name (ctid) AS SELECT 9");
    assert_eq!(
        exec(&engine, "SELECT ctid FROM system_name").rows[0]["ctid"],
        Value::Int(9)
    );
}

#[test]
fn view_column_names_and_types_are_visible_in_catalogs() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE source (small SMALLINT, label VARCHAR(3))",
    );
    exec(
        &engine,
        "CREATE VIEW catalog_view (renamed_small, \"Label\") AS SELECT small, label FROM source",
    );

    let info = exec(
        &engine,
        "SELECT column_name, ordinal_position, data_type, character_maximum_length, is_nullable, is_updatable
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'catalog_view'
         ORDER BY ordinal_position",
    );
    assert_eq!(info.rows.len(), 2);
    assert_eq!(
        info.rows[0]["column_name"],
        Value::Str("renamed_small".into())
    );
    assert_eq!(info.rows[0]["data_type"], Value::Str("smallint".into()));
    assert_eq!(info.rows[0]["is_nullable"], Value::Str("YES".into()));
    assert_eq!(info.rows[0]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(info.rows[1]["column_name"], Value::Str("Label".into()));
    assert_eq!(info.rows[1]["character_maximum_length"], Value::Int(3));

    let class = exec(
        &engine,
        "SELECT relnatts FROM pg_catalog.pg_class WHERE relname = 'catalog_view'",
    );
    assert_eq!(class.rows[0]["relnatts"], Value::Int(2));
    let attributes = exec(
        &engine,
        "SELECT a.attname, a.atttypid, a.atttypmod, a.attnotnull
         FROM pg_catalog.pg_attribute AS a
         JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid
         WHERE c.relname = 'catalog_view'
         ORDER BY a.attnum",
    );
    assert_eq!(attributes.rows.len(), 2);
    assert_eq!(
        attributes.rows[0]["attname"],
        Value::Str("renamed_small".into())
    );
    assert_eq!(attributes.rows[0]["atttypid"], Value::Int(21));
    assert_eq!(attributes.rows[0]["attnotnull"], Value::Bool(false));
    assert_eq!(attributes.rows[1]["attname"], Value::Str("Label".into()));
    assert_eq!(attributes.rows[1]["atttypid"], Value::Int(1043));
    assert_eq!(attributes.rows[1]["atttypmod"], Value::Int(7));
}

#[test]
fn create_view_analyzes_before_aliases_and_target_but_never_executes() {
    let engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(&engine, "CREATE TABLE occupied (value INTEGER)");

    let missing = engine
        .sql(
            "CREATE VIEW missing (same, same) AS SELECT a, b FROM no_such_relation",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42P01"));

    let invalid_cast = engine
        .sql(
            "CREATE VIEW occupied (same, same) AS SELECT 'bad'::integer, 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(invalid_cast.sqlstate(), Some("22P02"));

    let missing_function = engine
        .sql(
            "CREATE VIEW occupied (same, same) AS SELECT no_such_function(1), 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_function.sqlstate(), Some("42883"));

    let duplicate_on_existing_target = engine
        .sql("CREATE VIEW occupied (same, same) AS SELECT 1, 2", &[])
        .unwrap_err();
    assert_eq!(duplicate_on_existing_target.sqlstate(), Some("42701"));

    let duplicate = engine
        .sql(
            "CREATE VIEW side_effect (same, same) AS SELECT count_calls(), 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42701"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let occupied = engine
        .sql("CREATE VIEW occupied AS SELECT 1 AS value", &[])
        .unwrap_err();
    assert_eq!(occupied.sqlstate(), Some("42P07"));
    let wrong_kind = engine
        .sql("CREATE OR REPLACE VIEW occupied AS SELECT 1 AS value", &[])
        .unwrap_err();
    assert_eq!(wrong_kind.sqlstate(), Some("42809"));
}

#[test]
fn replace_view_preserves_existing_columns_and_may_append() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE VIEW replaceable (value, label) AS SELECT 7::smallint, 'old'::text",
    );

    for (sql, state) in [
        (
            "CREATE OR REPLACE VIEW replaceable (renamed, label) AS SELECT 8::smallint, 'new'::text",
            "42P16",
        ),
        (
            "CREATE OR REPLACE VIEW replaceable (value, label) AS SELECT 8::integer, 'new'::text",
            "42P16",
        ),
        (
            "CREATE OR REPLACE VIEW replaceable (value) AS SELECT 8::smallint",
            "42P16",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    assert_eq!(
        exec(&engine, "SELECT value, label FROM replaceable").rows[0]["value"],
        Value::Int(7)
    );

    exec(
        &engine,
        "CREATE OR REPLACE VIEW replaceable (value, label) AS SELECT 8::smallint, 'new'::text, 3::bigint AS added",
    );
    let replaced = exec(&engine, "SELECT value, label, added FROM replaceable");
    assert_eq!(replaced.columns, ["value", "label", "added"]);
    assert_eq!(replaced.rows[0]["value"], Value::Int(8));
    assert_eq!(replaced.rows[0]["added"], Value::Int(3));
}

#[test]
fn aliased_view_creation_does_not_execute_volatile_sequence_calls() {
    let engine = Engine::new();
    exec(&engine, "CREATE SEQUENCE view_alias_ticks START 41");
    let before = engine
        .sequence_state("view_alias_ticks")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(before.current, 41);
    assert!(!before.called);

    exec(
        &engine,
        "CREATE VIEW tick_view (tick) AS SELECT nextval('view_alias_ticks')",
    );
    let after_create = engine
        .sequence_state("view_alias_ticks")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(after_create.current, 41);
    assert!(!after_create.called);

    assert_eq!(
        exec(&engine, "SELECT tick FROM tick_view").rows[0]["tick"],
        Value::Int(41)
    );
    assert!(
        engine
            .sequence_state("view_alias_ticks")
            .unwrap()
            .unwrap()
            .1
            .called
    );
}

#[test]
fn aliased_views_are_transactional_and_survive_reopen_through_nested_views() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view-column-aliases.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE source (small SMALLINT, label VARCHAR(3))",
        );
        exec(&engine, "INSERT INTO source VALUES (7, 'abc')");
        exec(&engine, "BEGIN");
        exec(
            &engine,
            "CREATE VIEW rolled_back (value) AS SELECT small FROM source",
        );
        exec(&engine, "ROLLBACK");
        assert!(engine.view("rolled_back").unwrap().is_none());

        exec(
            &engine,
            "CREATE VIEW aliased (\"Renamed.Small\") AS SELECT small, label FROM source",
        );
        exec(
            &engine,
            "CREATE VIEW nested AS SELECT \"Renamed.Small\" AS value, label FROM aliased",
        );
    }

    let engine = Engine::open(&path).unwrap();
    assert!(engine.view("rolled_back").unwrap().is_none());
    let result = exec(&engine, "SELECT value, label FROM nested WHERE value = 7");
    assert_eq!(result.column_types[0], Some(ColumnType::SmallInteger));
    assert_eq!(result.rows[0]["value"], Value::Int(7));
    assert_eq!(result.rows[0]["label"], Value::Str("abc".into()));
}

#[test]
fn legacy_query_only_view_definitions_still_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-view-definition.db");
    let legacy_json = {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE TABLE source (value SMALLINT)");
        exec(&engine, "INSERT INTO source VALUES (7)");
        exec(
            &engine,
            "CREATE VIEW legacy_view AS SELECT value FROM source",
        );
        serde_json::to_string(&engine.view("legacy_view").unwrap().unwrap()).unwrap()
    };
    let connection = rusqlite::Connection::open(&path).unwrap();
    let updated_rows = connection
        .execute(
            "UPDATE _views SET definition_json = ?1 WHERE schema_name = 'public' AND relation_name = 'legacy_view'",
            [legacy_json],
        )
        .unwrap();
    assert_eq!(updated_rows, 1, "legacy view fixture must update one row");
    drop(connection);

    let engine = Engine::open(&path).unwrap();
    let result = exec(&engine, "SELECT value FROM legacy_view");
    assert_eq!(result.column_types, [Some(ColumnType::SmallInteger)]);
    assert_eq!(result.rows[0]["value"], Value::Int(7));
}
