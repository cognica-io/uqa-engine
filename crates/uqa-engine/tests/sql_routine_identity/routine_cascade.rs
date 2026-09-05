//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement must fail")
        .sqlstate()
        .expect("SQLSTATE")
        .to_string()
}

fn assert_routine_drops_restricted(engine: &Engine, targets: &[&str]) {
    for target in targets {
        assert_eq!(
            sqlstate(engine, &format!("DROP FUNCTION {target} RESTRICT")),
            "2BP01",
            "{target}"
        );
    }
}

fn rename_and_recreate_routines(engine: &Engine, names: &[(&str, &str)]) {
    for (old_name, new_name) in names {
        engine
            .sql(
                &format!("ALTER FUNCTION {old_name}(integer) RENAME TO {new_name}"),
                &[],
            )
            .unwrap();
        engine
            .sql(
                &format!(
                    "CREATE FUNCTION {old_name}(value integer) RETURNS integer RETURN value + 1000"
                ),
                &[],
            )
            .unwrap();
    }
}

#[test]
fn sql_standard_routine_dependencies_restrict_and_cascade_transitively() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION cascade_base(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION cascade_middle(i integer) RETURNS integer RETURN cascade_base(i)",
        "CREATE FUNCTION cascade_leaf(i integer) RETURNS integer RETURN cascade_middle(i)",
        "CREATE VIEW cascade_leaf_view AS SELECT cascade_leaf(1) AS value",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let error = engine
        .sql("DROP FUNCTION cascade_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    assert_eq!(
        scalar(&engine, "SELECT cascade_leaf(1) AS v"),
        Value::Int(2)
    );

    engine
        .sql("DROP FUNCTION cascade_base(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
    );
    for sql in [
        "SELECT cascade_base(1)",
        "SELECT cascade_middle(1)",
        "SELECT cascade_leaf(1)",
        "SELECT * FROM cascade_leaf_view",
    ] {
        assert!(engine.sql(sql, &[]).is_err(), "{sql}");
    }
}

#[test]
fn explicit_multi_target_drop_satisfies_internal_dependency_without_cascade() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION multi_base(i integer) RETURNS integer RETURN i + 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION multi_dep(i integer) RETURNS integer RETURN multi_base(i)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "DROP FUNCTION multi_base(integer), multi_dep(integer) RESTRICT",
            &[],
        )
        .unwrap();
    assert_eq!(sqlstate(&engine, "SELECT multi_base(1)"), "42883");
    assert_eq!(sqlstate(&engine, "SELECT multi_dep(1)"), "42883");
}

#[test]
fn sql_string_body_keeps_postgresql_dynamic_dependency_behavior() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION dynamic_base(i integer) RETURNS integer RETURN i + 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION dynamic_dep(i integer) RETURNS integer LANGUAGE SQL AS 'SELECT dynamic_base($1)'",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP FUNCTION dynamic_base(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(sqlstate(&engine, "SELECT dynamic_dep(1)"), "42883");
}

#[test]
fn standard_body_positional_parameter_binds_the_exact_overload() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION positional_base(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION positional_base(i bigint) RETURNS bigint RETURN i + 2",
        "CREATE FUNCTION positional_dep(i integer) RETURNS integer RETURN positional_base($1)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    assert_eq!(
        scalar(&engine, "SELECT positional_dep(1) AS v"),
        Value::Int(2)
    );
    engine
        .sql("DROP FUNCTION positional_base(bigint) RESTRICT", &[])
        .unwrap();
    let error = engine
        .sql("DROP FUNCTION positional_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn standard_body_replacement_atomically_changes_its_dependency_set() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION replace_base_old(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION replace_base_new(i integer) RETURNS integer RETURN i + 2",
        "CREATE FUNCTION replace_dep(i integer) RETURNS integer RETURN replace_base_old(i)",
        "CREATE OR REPLACE FUNCTION replace_dep(i integer) RETURNS integer RETURN replace_base_new(i)",
    ] {
        engine.sql(ddl, &[]).unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    engine
        .sql("DROP FUNCTION replace_base_old(integer) RESTRICT", &[])
        .unwrap();
    let error = engine
        .sql("DROP FUNCTION replace_base_new(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");

    let error = engine
        .sql(
            "CREATE OR REPLACE FUNCTION replace_dep(i integer) RETURNS integer RETURN missing_replace_target(i)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"), "{error}");
    assert_eq!(scalar(&engine, "SELECT replace_dep(1) AS v"), Value::Int(3));
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION replace_base_new(integer) RESTRICT"),
        "2BP01"
    );
}

#[test]
fn standard_body_keeps_dependencies_in_statically_unreachable_expressions() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION unreachable_dependency_base(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION unreachable_dependency_caller() RETURNS integer RETURN CASE WHEN false THEN unreachable_dependency_base(1) ELSE 0 END",
            &[],
        )
        .unwrap();

    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION unreachable_dependency_base(integer) RESTRICT"
        ),
        "2BP01"
    );
}

#[test]
fn parameter_default_replacement_atomically_changes_its_dependency_set() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION default_replace_old(value integer) RETURNS integer RETURN value + 1",
        "CREATE FUNCTION default_replace_new(value integer) RETURNS integer RETURN value + 2",
        "CREATE FUNCTION default_replace(value integer DEFAULT default_replace_old(1)) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
        "CREATE OR REPLACE FUNCTION default_replace(value integer DEFAULT default_replace_new(1)) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    engine
        .sql("DROP FUNCTION default_replace_old(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION default_replace_new(integer) RESTRICT"
        ),
        "2BP01"
    );

    let error = engine
        .sql(
            "CREATE OR REPLACE FUNCTION default_replace(value integer DEFAULT missing_default_target(1)) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"), "{error}");
    assert_eq!(
        scalar(&engine, "SELECT default_replace() AS v"),
        Value::Int(3)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION default_replace_new(integer) RESTRICT"
        ),
        "2BP01"
    );
}

#[test]
fn dependent_sql_procedure_and_durable_reopen_follow_the_same_graph() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-cascade.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE FUNCTION durable_base(i integer) RETURNS integer RETURN i + 1",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE PROCEDURE durable_proc(i integer) LANGUAGE SQL BEGIN ATOMIC SELECT durable_base(i); END",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let error = reopened
        .sql("DROP FUNCTION durable_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    reopened
        .sql("DROP FUNCTION durable_base(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        reopened.take_sql_notices(),
        vec![(
            "NOTICE".into(),
            "drop cascades to procedure public.durable_proc(integer)".into(),
        )]
    );
    assert_eq!(sqlstate(&reopened, "CALL durable_proc(1)"), "42883");
}

#[test]
fn durable_standard_body_keeps_its_creation_search_path_binding() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-search-path-cascade.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SCHEMA early",
            "CREATE SCHEMA late",
            "CREATE TABLE early.bound_rows(value integer)",
            "INSERT INTO early.bound_rows VALUES (1)",
            "CREATE TABLE late.bound_rows(value integer)",
            "INSERT INTO late.bound_rows VALUES (2)",
            "CREATE FUNCTION early.bound_target(i integer) RETURNS integer RETURN i + 10",
            "CREATE FUNCTION late.bound_target(i integer) RETURNS integer RETURN i + 20",
            "SET search_path TO early, late, public",
            "CREATE FUNCTION bound_dep(i integer) RETURNS integer RETURN bound_target(i)",
            "CREATE FUNCTION bound_relation_dep() RETURNS integer RETURN (SELECT value FROM bound_rows)",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql("SET search_path TO late, early, public", &[])
        .unwrap();
    reopened
        .sql("DROP FUNCTION late.bound_target(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT early.bound_dep(1) AS v"),
        Value::Int(11)
    );
    assert_eq!(
        scalar(&reopened, "SELECT early.bound_relation_dep() AS v"),
        Value::Int(1)
    );

    let error = reopened
        .sql("DROP FUNCTION early.bound_target(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    reopened
        .sql("DROP FUNCTION early.bound_target(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(sqlstate(&reopened, "SELECT early.bound_dep(1)"), "42883");
}

#[test]
fn durable_standard_body_can_bind_a_stored_view_during_restore() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-view-body-dependency.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE TABLE routine_view_source(value integer)",
            "INSERT INTO routine_view_source VALUES (7)",
            "CREATE FUNCTION routine_view_transform(value integer) RETURNS integer RETURN value + 1",
            "CREATE VIEW routine_body_view AS SELECT routine_view_transform(value) AS value FROM routine_view_source",
            "CREATE FUNCTION routine_view_reader() RETURNS integer RETURN (SELECT value FROM routine_body_view)",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        assert_eq!(
            scalar(&engine, "SELECT routine_view_reader() AS v"),
            Value::Int(8)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT routine_view_reader() AS v"),
        Value::Int(8)
    );
    let secondary = reopened.new_session().unwrap();
    assert_eq!(
        scalar(&secondary, "SELECT routine_view_reader() AS v"),
        Value::Int(8)
    );
}

#[test]
fn command_body_dependency_survives_rename_recreation_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-command-dependency.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE TABLE command_sink(value integer)",
            "CREATE TABLE command_merge_target(id integer PRIMARY KEY, value integer)",
            "CREATE TABLE command_merge_source(id integer, value integer)",
            "INSERT INTO command_merge_source VALUES (1, 5)",
            "CREATE FUNCTION command_base(value integer) RETURNS integer RETURN value + 10",
            "CREATE PROCEDURE command_writer(value integer) LANGUAGE SQL BEGIN ATOMIC INSERT INTO command_sink VALUES (command_base(value)); UPDATE command_sink SET value = command_base(command_sink.value) WHERE false; DELETE FROM command_sink WHERE command_base(command_sink.value) < 0; END",
            "CREATE PROCEDURE command_merge_writer() LANGUAGE SQL BEGIN ATOMIC MERGE INTO command_merge_target AS target USING command_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = command_base(source.value) WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, command_base(source.value)); END",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        assert_eq!(
            sqlstate(&engine, "DROP FUNCTION command_base(integer) RESTRICT"),
            "2BP01"
        );
        engine
            .sql(
                "ALTER FUNCTION command_base(integer) RENAME TO command_base_renamed",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION command_base(value integer) RETURNS integer RETURN value + 1000",
                &[],
            )
            .unwrap();
        engine.sql("CALL command_writer(2)", &[]).unwrap();
        engine.sql("CALL command_merge_writer()", &[]).unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT value AS v FROM command_sink WHERE value = 12"
            ),
            Value::Int(12)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT value AS v FROM command_merge_target WHERE id = 1"
            ),
            Value::Int(15)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    reopened.sql("CALL command_writer(3)", &[]).unwrap();
    reopened
        .sql(
            "UPDATE command_merge_source SET value = 6 WHERE id = 1",
            &[],
        )
        .unwrap();
    reopened.sql("CALL command_merge_writer()", &[]).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT value AS v FROM command_sink WHERE value = 13"
        ),
        Value::Int(13)
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT value AS v FROM command_merge_target WHERE id = 1"
        ),
        Value::Int(16)
    );
    reopened
        .sql("DROP FUNCTION command_base(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION command_base_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );
    reopened
        .sql("DROP FUNCTION command_base_renamed(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        reopened.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 2 other objects".into())]
    );
    assert_eq!(sqlstate(&reopened, "CALL command_writer(4)"), "42883");
    assert_eq!(sqlstate(&reopened, "CALL command_merge_writer()"), "42883");
}

#[test]
fn merge_command_body_binds_routines_in_stored_statement_order() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-merge-binding-order.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE TABLE merge_binding_target(id integer PRIMARY KEY, value integer)",
            "CREATE TABLE merge_binding_source(id integer, value integer)",
            "INSERT INTO merge_binding_source VALUES (1, 5)",
            "CREATE FUNCTION merge_binding_join(value integer) RETURNS integer RETURN value",
            "CREATE FUNCTION merge_binding_update(value integer) RETURNS integer RETURN value + 10",
            "CREATE FUNCTION merge_binding_return(value integer) RETURNS integer RETURN value + 100",
            "CREATE PROCEDURE merge_binding_writer() LANGUAGE SQL BEGIN ATOMIC MERGE INTO merge_binding_target AS target USING merge_binding_source AS source ON target.id = merge_binding_join(source.id) WHEN MATCHED THEN UPDATE SET value = merge_binding_update(source.value) WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, merge_binding_update(source.value)) RETURNING merge_binding_return(target.value); END",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        for target in [
            "merge_binding_join(integer)",
            "merge_binding_update(integer)",
            "merge_binding_return(integer)",
        ] {
            assert_eq!(
                sqlstate(&engine, &format!("DROP FUNCTION {target} RESTRICT")),
                "2BP01",
                "{target}"
            );
        }
        engine
            .sql(
                "ALTER FUNCTION merge_binding_update(integer) RENAME TO merge_binding_update_renamed",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION merge_binding_update(value integer) RETURNS integer RETURN value + 1000",
                &[],
            )
            .unwrap();
        engine.sql("CALL merge_binding_writer()", &[]).unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT value AS v FROM merge_binding_target WHERE id = 1"
            ),
            Value::Int(15)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql(
            "UPDATE merge_binding_source SET value = 6 WHERE id = 1",
            &[],
        )
        .unwrap();
    reopened.sql("CALL merge_binding_writer()", &[]).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT value AS v FROM merge_binding_target WHERE id = 1"
        ),
        Value::Int(16)
    );
    reopened
        .sql("DROP FUNCTION merge_binding_update(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION merge_binding_update_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );
}

#[test]
fn each_mutation_command_body_keeps_its_exact_routine_binding() {
    let engine = Engine::new();
    for ddl in [
        "CREATE TABLE mutation_binding_target(id integer PRIMARY KEY, value integer)",
        "CREATE TABLE mutation_delete_target(id integer PRIMARY KEY, value integer)",
        "CREATE TABLE mutation_merge_target(id integer PRIMARY KEY, value integer)",
        "CREATE TABLE mutation_binding_source(id integer, value integer)",
        "INSERT INTO mutation_delete_target VALUES (-1, 5)",
        "INSERT INTO mutation_binding_source VALUES (1, 7)",
        "CREATE FUNCTION mutation_insert(value integer) RETURNS integer RETURN value + 10",
        "CREATE FUNCTION mutation_update(value integer) RETURNS integer RETURN value + 20",
        "CREATE FUNCTION mutation_delete(value integer) RETURNS integer RETURN value - 100",
        "CREATE FUNCTION mutation_merge(value integer) RETURNS integer RETURN value + 30",
        "CREATE PROCEDURE mutation_writer(input integer) LANGUAGE SQL BEGIN ATOMIC INSERT INTO mutation_binding_target VALUES (input, mutation_insert(input)); UPDATE mutation_binding_target SET value = mutation_update(value) WHERE id = input; DELETE FROM mutation_delete_target WHERE mutation_delete(value) < 0; MERGE INTO mutation_merge_target AS target USING mutation_binding_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = mutation_merge(source.value) WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, mutation_merge(source.value)); END",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    assert_routine_drops_restricted(
        &engine,
        &[
            "mutation_insert(integer)",
            "mutation_update(integer)",
            "mutation_delete(integer)",
            "mutation_merge(integer)",
        ],
    );
    rename_and_recreate_routines(
        &engine,
        &[
            ("mutation_insert", "mutation_insert_renamed"),
            ("mutation_update", "mutation_update_renamed"),
            ("mutation_delete", "mutation_delete_renamed"),
            ("mutation_merge", "mutation_merge_renamed"),
        ],
    );

    engine.sql("CALL mutation_writer(2)", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT value AS v FROM mutation_binding_target WHERE id = 2"
        ),
        Value::Int(32)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT value AS v FROM mutation_merge_target WHERE id = 1"
        ),
        Value::Int(37)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM mutation_delete_target WHERE id = -1"
        ),
        Value::Int(0)
    );
    for target in [
        "mutation_insert(integer)",
        "mutation_update(integer)",
        "mutation_delete(integer)",
        "mutation_merge(integer)",
    ] {
        engine
            .sql(&format!("DROP FUNCTION {target} RESTRICT"), &[])
            .unwrap();
    }
    assert_routine_drops_restricted(
        &engine,
        &[
            "mutation_insert_renamed(integer)",
            "mutation_update_renamed(integer)",
            "mutation_delete_renamed(integer)",
            "mutation_merge_renamed(integer)",
        ],
    );
    engine
        .sql(
            "DROP FUNCTION mutation_insert_renamed(integer) CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(sqlstate(&engine, "CALL mutation_writer(3)"), "42883");
    for target in [
        "mutation_update_renamed(integer)",
        "mutation_delete_renamed(integer)",
        "mutation_merge_renamed(integer)",
    ] {
        engine
            .sql(&format!("DROP FUNCTION {target} RESTRICT"), &[])
            .unwrap();
    }
}

#[test]
fn parameter_default_dependencies_bind_every_body_form_and_creation_path() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-default-dependency.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SCHEMA early",
            "CREATE SCHEMA late",
            "CREATE FUNCTION early.default_base(value integer) RETURNS integer RETURN value + 10",
            "CREATE FUNCTION late.default_base(value integer) RETURNS integer RETURN value + 100",
            "SET search_path TO early, late, public",
            "CREATE FUNCTION default_standard(value integer DEFAULT default_base(1)) RETURNS integer RETURN value",
            "CREATE FUNCTION default_source(value integer DEFAULT default_base(2)) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
            "CREATE FUNCTION default_plpgsql(value integer DEFAULT default_base(3)) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN value; END $$",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FUNCTION early.default_base(integer) RESTRICT"
            ),
            "2BP01"
        );
        engine
            .sql(
                "ALTER FUNCTION early.default_base(integer) RENAME TO default_base_renamed",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION early.default_base(value integer) RETURNS integer RETURN value + 1000",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql("SET search_path TO late, early, public", &[])
        .unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT early.default_standard() AS v"),
        Value::Int(11)
    );
    assert_eq!(
        scalar(&reopened, "SELECT early.default_source() AS v"),
        Value::Int(12)
    );
    assert_eq!(
        scalar(&reopened, "SELECT early.default_plpgsql() AS v"),
        Value::Int(13)
    );
    reopened
        .sql("DROP FUNCTION late.default_base(integer) RESTRICT", &[])
        .unwrap();
    reopened
        .sql("DROP FUNCTION early.default_base(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION early.default_base_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );
    reopened
        .sql(
            "DROP FUNCTION early.default_base_renamed(integer) CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        reopened.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
    );
    for sql in [
        "SELECT early.default_standard()",
        "SELECT early.default_source()",
        "SELECT early.default_plpgsql()",
    ] {
        assert_eq!(sqlstate(&reopened, sql), "42883", "{sql}");
    }
}

#[test]
fn table_defaults_and_checks_keep_exact_routine_dependencies() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 10",
        "CREATE FUNCTION schema_dep(value text) RETURNS text IMMUTABLE RETURN value || '!'",
        "CREATE TABLE schema_dependency_rows (id integer DEFAULT schema_dep(1), value integer CONSTRAINT value_dep_check CHECK (schema_dep(value) < 100), other integer, CONSTRAINT other_dep_check CHECK (schema_dep(other) < 100))",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("DROP FUNCTION schema_dep(text) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION schema_dep(integer) RESTRICT"),
        "2BP01"
    );
    engine
        .sql(
            "ALTER FUNCTION schema_dep(integer) RENAME TO schema_dep_renamed",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 1000",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO schema_dependency_rows(value, other) VALUES (1, 2)",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT id AS v FROM schema_dependency_rows WHERE value = 1"
        ),
        Value::Int(11)
    );
    engine
        .sql("DROP FUNCTION schema_dep(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION schema_dep_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );

    engine
        .sql("DROP FUNCTION schema_dep_renamed(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
    );
    assert!(engine
        .column_default_expr("schema_dependency_rows", "id")
        .unwrap()
        .is_none());
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint WHERE conname IN ('value_dep_check', 'other_dep_check')"
        ),
        Value::Int(0)
    );
    assert_eq!(
        engine.table_columns("schema_dependency_rows").unwrap(),
        vec!["id", "value", "other"]
    );
    engine
        .sql(
            "INSERT INTO schema_dependency_rows(value, other) VALUES (-1000, -1000)",
            &[],
        )
        .unwrap();
}

#[test]
fn altered_defaults_and_checks_replace_their_dependency_sets() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION old_schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FUNCTION new_schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 2",
        "CREATE TABLE altered_schema_dependencies (id integer DEFAULT old_schema_dep(1), value integer, CONSTRAINT old_schema_check CHECK (old_schema_dep(value) > 0))",
        "ALTER TABLE altered_schema_dependencies ALTER COLUMN id SET DEFAULT new_schema_dep(1)",
        "ALTER TABLE altered_schema_dependencies DROP CONSTRAINT old_schema_check",
        "ALTER TABLE altered_schema_dependencies ADD CONSTRAINT new_schema_check CHECK (new_schema_dep(value) > 0)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("DROP FUNCTION old_schema_dep(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION new_schema_dep(integer) RESTRICT"),
        "2BP01"
    );
    engine
        .sql(
            "ALTER TABLE altered_schema_dependencies ALTER COLUMN id DROP DEFAULT",
            &[],
        )
        .unwrap();
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION new_schema_dep(integer) RESTRICT"),
        "2BP01"
    );
    engine
        .sql(
            "ALTER TABLE altered_schema_dependencies DROP CONSTRAINT new_schema_check",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP FUNCTION new_schema_dep(integer) RESTRICT", &[])
        .unwrap();
}

#[test]
fn schema_expression_routine_bindings_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("schema-routine-dependencies.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE FUNCTION durable_schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 10",
            "CREATE TABLE durable_schema_rows (id integer DEFAULT durable_schema_dep(1), value integer CONSTRAINT durable_schema_check CHECK (durable_schema_dep(value) < 100))",
            "ALTER FUNCTION durable_schema_dep(integer) RENAME TO durable_schema_dep_renamed",
            "CREATE FUNCTION durable_schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 1000",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
    }

    {
        let reopened = Engine::open(&database).unwrap();
        reopened
            .sql("INSERT INTO durable_schema_rows(value) VALUES (1)", &[])
            .unwrap();
        assert_eq!(
            scalar(&reopened, "SELECT id AS v FROM durable_schema_rows"),
            Value::Int(11)
        );
        reopened
            .sql("DROP FUNCTION durable_schema_dep(integer) RESTRICT", &[])
            .unwrap();
        assert_eq!(
            sqlstate(
                &reopened,
                "DROP FUNCTION durable_schema_dep_renamed(integer) RESTRICT"
            ),
            "2BP01"
        );
        reopened
            .sql(
                "DROP FUNCTION durable_schema_dep_renamed(integer) CASCADE",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened
        .column_default_expr("durable_schema_rows", "id")
        .unwrap()
        .is_none());
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint WHERE conname = 'durable_schema_check'"
        ),
        Value::Int(0)
    );
}

#[test]
fn schema_expression_cascade_rolls_back_as_one_catalog_change() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION rollback_schema_dep(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE TABLE rollback_schema_rows(id integer DEFAULT rollback_schema_dep(1), value integer CONSTRAINT rollback_schema_check CHECK (rollback_schema_dep(value) > 0))",
        "BEGIN",
        "DROP FUNCTION rollback_schema_dep(integer) CASCADE",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    assert!(engine
        .column_default_expr("rollback_schema_rows", "id")
        .unwrap()
        .is_none());
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint WHERE conname = 'rollback_schema_check'"
        ),
        Value::Int(0)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(engine
        .column_default_expr("rollback_schema_rows", "id")
        .unwrap()
        .is_some());
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint WHERE conname = 'rollback_schema_check'"
        ),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION rollback_schema_dep(integer) RESTRICT"
        ),
        "2BP01"
    );
}
