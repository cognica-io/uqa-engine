//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::SequenceOwnerDependency;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement must fail")
        .sqlstate()
        .expect("SQLSTATE")
        .to_string()
}

fn remove_expression_function_bindings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = fields
                .get_mut("Func")
                .and_then(serde_json::Value::as_object_mut)
                .map_or(0, |function| {
                    usize::from(function.remove("binding").is_some())
                });
            for value in fields.values_mut() {
                removed += remove_expression_function_bindings(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(remove_expression_function_bindings)
            .sum(),
        _ => 0,
    }
}

fn assert_foreign_sequence_dependencies_removed(engine: &Engine) {
    assert_eq!(
        scalar(
            engine,
            "SELECT to_regclass('foreign_sequence_dependency_renamed') IS NULL AND to_regclass('foreign_sequence_items') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT attnotnull AND NOT atthasdef AS v FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'foreign_sequence_items' AND attribute_row.attname = 'id'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'foreign_sequence_items' AND constraint_row.contype = 'c'"
        ),
        Value::Int(0)
    );
}

fn assert_sequence_owner_dependency(
    engine: &Engine,
    sequence: &str,
    expected: SequenceOwnerDependency,
) {
    let owner = engine
        .sequence_state(sequence)
        .unwrap()
        .unwrap()
        .1
        .owner
        .unwrap();
    assert_eq!(owner.dependency, expected);
}

#[test]
fn foreign_table_defaults_and_checks_keep_exact_routine_dependencies() {
    let engine = Engine::new();
    for ddl in [
        "CREATE SERVER foreign_dependency_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FUNCTION foreign_dependency(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FUNCTION foreign_dependency(value text) RETURNS integer IMMUTABLE RETURN 0",
        "CREATE FOREIGN TABLE foreign_dependency_items (id integer NOT NULL DEFAULT foreign_dependency(1) CHECK (foreign_dependency(id) > 0), qty integer, CONSTRAINT foreign_dependency_qty_check CHECK (foreign_dependency(qty) > 0)) SERVER foreign_dependency_server OPTIONS (source 'memory')",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("DROP FUNCTION foreign_dependency(text) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION foreign_dependency(integer) RESTRICT"
        ),
        "2BP01"
    );
    engine
        .sql(
            "ALTER FUNCTION foreign_dependency(integer) RENAME TO foreign_dependency_renamed",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION foreign_dependency(value integer) RETURNS integer IMMUTABLE RETURN 0",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT position('foreign_dependency_renamed' in column_default) > 0 AS v FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'foreign_dependency_items' AND column_name = 'id'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'foreign_dependency_items' AND constraint_row.contype = 'c'"
        ),
        Value::Int(2)
    );
    engine
        .sql("DROP FUNCTION foreign_dependency(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION foreign_dependency_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );
    engine
        .sql(
            "DROP FUNCTION foreign_dependency_renamed(integer) CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('foreign_dependency_items') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attnotnull AND NOT atthasdef AS v FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'foreign_dependency_items' AND attribute_row.attname = 'id'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'foreign_dependency_items' AND constraint_row.contype = 'c'"
        ),
        Value::Int(0)
    );
}

#[test]
fn foreign_table_routine_dependencies_survive_reopen_and_rollback() {
    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("foreign-table-routine-dependency.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SERVER durable_foreign_dependency_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
            "CREATE FUNCTION durable_foreign_dependency(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
            "CREATE FOREIGN TABLE durable_foreign_dependency_items (id integer NOT NULL DEFAULT durable_foreign_dependency(1) CHECK (durable_foreign_dependency(id) > 0), qty integer, CONSTRAINT durable_foreign_dependency_qty_check CHECK (durable_foreign_dependency(qty) > 0)) SERVER durable_foreign_dependency_server OPTIONS (source 'memory')",
            "ALTER FUNCTION durable_foreign_dependency(integer) RENAME TO durable_foreign_dependency_renamed",
            "CREATE FUNCTION durable_foreign_dependency(value integer) RETURNS integer IMMUTABLE RETURN 0",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
    }

    {
        let engine = Engine::open(&database).unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT position('durable_foreign_dependency_renamed' in column_default) > 0 AS v FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'durable_foreign_dependency_items' AND column_name = 'id'"
            ),
            Value::Bool(true)
        );
        engine
            .sql(
                "DROP FUNCTION durable_foreign_dependency(integer) RESTRICT",
                &[],
            )
            .unwrap();
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FUNCTION durable_foreign_dependency_renamed(integer) RESTRICT"
            ),
            "2BP01"
        );
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                "DROP FUNCTION durable_foreign_dependency_renamed(integer) CASCADE",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'durable_foreign_dependency_items' AND constraint_row.contype = 'c'"
            ),
            Value::Int(0)
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FUNCTION durable_foreign_dependency_renamed(integer) RESTRICT"
            ),
            "2BP01"
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'durable_foreign_dependency_items' AND constraint_row.contype = 'c'"
            ),
            Value::Int(2)
        );
        engine
            .sql(
                "DROP FUNCTION durable_foreign_dependency_renamed(integer) CASCADE",
                &[],
            )
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('durable_foreign_dependency_items') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attnotnull AND NOT atthasdef AS v FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'durable_foreign_dependency_items' AND attribute_row.attname = 'id'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'durable_foreign_dependency_items' AND constraint_row.contype = 'c'"
        ),
        Value::Int(0)
    );
}

#[test]
fn foreign_generated_columns_follow_routine_lifecycle() {
    let engine = Engine::new();
    for ddl in [
        "CREATE SERVER foreign_generated_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FUNCTION foreign_generated_dependency(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FOREIGN TABLE foreign_generated_items (base integer, derived integer GENERATED ALWAYS AS (foreign_generated_dependency(base)) STORED) SERVER foreign_generated_server OPTIONS (source 'memory')",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION foreign_generated_dependency(integer) RESTRICT"
        ),
        "2BP01"
    );
    engine
        .sql(
            "ALTER FUNCTION foreign_generated_dependency(integer) RENAME TO foreign_generated_dependency_renamed",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT position('foreign_generated_dependency_renamed' in pg_get_expr(default_row.adbin, default_row.adrelid)) > 0 AS v FROM pg_catalog.pg_attrdef AS default_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = default_row.adrelid JOIN pg_catalog.pg_attribute AS attribute_row ON attribute_row.attrelid = default_row.adrelid AND attribute_row.attnum = default_row.adnum WHERE relation_row.relname = 'foreign_generated_items' AND attribute_row.attname = 'derived'"
        ),
        Value::Bool(true)
    );
    engine
        .sql(
            "DROP FUNCTION foreign_generated_dependency_renamed(integer) CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".into(),
            "drop cascades to column derived of foreign table public.foreign_generated_items"
                .into()
        )]
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'foreign_generated_items'"
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('foreign_generated_items') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
}

#[test]
fn foreign_table_sequence_dependencies_follow_rename_drop_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("foreign-table-sequence-dependency.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SERVER foreign_sequence_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
            "CREATE SEQUENCE foreign_sequence_dependency",
            "CREATE FOREIGN TABLE foreign_sequence_items (id bigint NOT NULL DEFAULT nextval('foreign_sequence_dependency'), qty bigint CHECK (qty < nextval('foreign_sequence_dependency')), CONSTRAINT foreign_sequence_table_check CHECK (id < nextval('foreign_sequence_dependency'))) SERVER foreign_sequence_server OPTIONS (source 'memory')",
            "ALTER SEQUENCE foreign_sequence_dependency RENAME TO foreign_sequence_dependency_renamed",
            "CREATE SEQUENCE foreign_sequence_dependency",
            "DROP SEQUENCE foreign_sequence_dependency RESTRICT",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        assert_eq!(
            scalar(
                &engine,
                "SELECT position('foreign_sequence_dependency_renamed' in column_default) > 0 AS v FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'foreign_sequence_items' AND column_name = 'id'"
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'foreign_sequence_items' AND constraint_row.contype = 'c'"
            ),
            Value::Int(2)
        );
        assert_eq!(
            sqlstate(
                &engine,
                "DROP SEQUENCE foreign_sequence_dependency_renamed RESTRICT"
            ),
            "2BP01"
        );
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                "DROP SEQUENCE foreign_sequence_dependency_renamed CASCADE",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT NOT atthasdef AS v FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'foreign_sequence_items' AND attribute_row.attname = 'id'"
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = constraint_row.conrelid WHERE relation_row.relname = 'foreign_sequence_items' AND constraint_row.contype = 'c'"
            ),
            Value::Int(0)
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        engine.take_sql_notices();
        assert_eq!(
            sqlstate(
                &engine,
                "DROP SEQUENCE foreign_sequence_dependency_renamed RESTRICT"
            ),
            "2BP01"
        );
        engine
            .sql(
                "DROP SEQUENCE foreign_sequence_dependency_renamed CASCADE",
                &[],
            )
            .unwrap();
        assert_eq!(
            engine.take_sql_notices(),
            vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
        );
    }

    assert_foreign_sequence_dependencies_removed(&Engine::open(&database).unwrap());
}

#[test]
fn foreign_table_legacy_schema_migration_is_initial_open_only_and_atomic() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("foreign-table-schema-migration.sqlite");
    let engine = Engine::open(&database).unwrap();
    for ddl in [
        "CREATE SERVER migration_foreign_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FUNCTION migration_foreign_dependency(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FOREIGN TABLE migration_foreign_items (id integer DEFAULT migration_foreign_dependency(1) CHECK (migration_foreign_dependency(id) > 0)) SERVER migration_foreign_server OPTIONS (source 'memory')",
        "CREATE TABLE migration_foreign_rule_source(value integer)",
        "CREATE TABLE migration_foreign_rule_log(value integer)",
        "CREATE RULE migration_foreign_rule AS ON INSERT TO migration_foreign_rule_source DO ALSO INSERT INTO migration_foreign_rule_log VALUES (NEW.value)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let mut foreign_tables = catalog.load_foreign_tables().unwrap();
    let table = foreign_tables
        .iter_mut()
        .find(|table| table.relation.name == "migration_foreign_items")
        .unwrap();
    let mut schema: serde_json::Value = serde_json::from_str(&table.columns_json).unwrap();
    assert_eq!(schema["version"], 1);
    let mut columns = schema["columns"].take();
    assert_eq!(remove_expression_function_bindings(&mut columns), 2);
    let legacy_schema = serde_json::to_string(&columns).unwrap();
    table.columns_json.clone_from(&legacy_schema);
    catalog.save_foreign_table(table).unwrap();

    let Err(error) = engine.new_session() else {
        panic!("secondary session must not repair a foreign-table schema");
    };
    assert!(error.to_string().contains("initial-open migration"));
    assert_eq!(
        catalog
            .load_foreign_tables()
            .unwrap()
            .into_iter()
            .find(|table| table.relation.name == "migration_foreign_items")
            .unwrap()
            .columns_json,
        legacy_schema
    );
    let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
    catalog.set_metadata("sql_rules_json", "{").unwrap();
    drop(catalog);
    drop(engine);

    assert!(Engine::open(&database).is_err());
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    assert_eq!(
        catalog
            .load_foreign_tables()
            .unwrap()
            .into_iter()
            .find(|table| table.relation.name == "migration_foreign_items")
            .unwrap()
            .columns_json,
        legacy_schema
    );
    catalog.set_metadata("sql_rules_json", &rules).unwrap();
    drop(catalog);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION migration_foreign_dependency(integer) RESTRICT"
        ),
        "2BP01"
    );
    drop(reopened);
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let migrated = catalog
        .load_foreign_tables()
        .unwrap()
        .into_iter()
        .find(|table| table.relation.name == "migration_foreign_items")
        .unwrap()
        .columns_json;
    let migrated: serde_json::Value = serde_json::from_str(&migrated).unwrap();
    assert_eq!(migrated["version"], 1);
    assert!(migrated["columns"].to_string().contains("\"binding\""));
}

#[test]
fn invalid_foreign_table_schema_never_reaches_the_catalog() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SERVER invalid_foreign_schema_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
            &[],
        )
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FOREIGN TABLE invalid_foreign_default (id integer DEFAULT missing_foreign_dependency(1)) SERVER invalid_foreign_schema_server"
        ),
        "42883"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('invalid_foreign_default') IS NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FOREIGN TABLE invalid_foreign_check (id integer, CHECK (missing_column > 0)) SERVER invalid_foreign_schema_server"
        ),
        "42703"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('invalid_foreign_check') IS NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FOREIGN TABLE invalid_foreign_generated_sequence (id serial, value integer CHECK (missing_column > 0)) SERVER invalid_foreign_schema_server"
        ),
        "42703"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('invalid_foreign_generated_sequence') IS NULL AND to_regclass('invalid_foreign_generated_sequence_id_seq') IS NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FOREIGN TABLE missing_foreign_server (id integer) SERVER absent_foreign_server"
        ),
        "42704"
    );
    engine
        .sql("CREATE TABLE foreign_relation_collision(id integer)", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FOREIGN TABLE foreign_relation_collision(id integer) SERVER invalid_foreign_schema_server"
        ),
        "42P07"
    );
    engine
        .sql(
            "CREATE FOREIGN TABLE IF NOT EXISTS foreign_relation_collision(id integer) SERVER absent_foreign_server",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".into(),
            "relation \"foreign_relation_collision\" already exists, skipping".into()
        )]
    );
}

#[test]
fn implicit_foreign_sequence_names_avoid_existing_relations() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SERVER foreign_generated_sequence_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
             CREATE SEQUENCE foreign_generated_sequence_collision_id_seq;
             CREATE TABLE foreign_generated_sequence_collision_id_seq1(marker integer);
             CREATE FOREIGN TABLE foreign_generated_sequence_collision (id serial) SERVER foreign_generated_sequence_server",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT pg_get_serial_sequence('foreign_generated_sequence_collision', 'id') AS v",
        ),
        Value::Str("public.foreign_generated_sequence_collision_id_seq2".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT (SELECT relkind = 'S' FROM pg_catalog.pg_class WHERE relname = 'foreign_generated_sequence_collision_id_seq') AND (SELECT relkind = 'r' FROM pg_catalog.pg_class WHERE relname = 'foreign_generated_sequence_collision_id_seq1') AS v",
        ),
        Value::Bool(true)
    );
}

#[test]
fn foreign_serial_and_identity_use_owned_sequence_objects() {
    let engine = Engine::new();
    for ddl in [
        "CREATE SERVER foreign_generated_sequence_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE foreign_generated_sequence_items (serial_id serial, identity_id bigint GENERATED ALWAYS AS IDENTITY, manual_id integer) SERVER foreign_generated_sequence_server",
        "CREATE SEQUENCE foreign_manual_sequence OWNED BY foreign_generated_sequence_items.manual_id",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    for (sequence, dependency) in [
        (
            "foreign_generated_sequence_items_serial_id_seq",
            SequenceOwnerDependency::Automatic,
        ),
        (
            "foreign_generated_sequence_items_identity_id_seq",
            SequenceOwnerDependency::Internal,
        ),
    ] {
        assert_sequence_owner_dependency(&engine, sequence, dependency);
    }
    for (column, sequence) in [
        (
            "serial_id",
            "public.foreign_generated_sequence_items_serial_id_seq",
        ),
        (
            "identity_id",
            "public.foreign_generated_sequence_items_identity_id_seq",
        ),
        ("manual_id", "public.foreign_manual_sequence"),
    ] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT pg_get_serial_sequence('foreign_generated_sequence_items', '{column}') AS v"
                ),
            ),
            Value::Str(sequence.into())
        );
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT is_identity = 'YES' AND identity_generation = 'ALWAYS' AS v FROM information_schema.columns WHERE table_name = 'foreign_generated_sequence_items' AND column_name = 'identity_id'"
        ),
        Value::Bool(true)
    );
    let identity_error = engine
        .sql(
            "DROP SEQUENCE foreign_generated_sequence_items_identity_id_seq CASCADE",
            &[],
        )
        .expect_err("an identity-owned sequence cannot be dropped directly");
    assert_eq!(identity_error.sqlstate(), Some("2BP01"));
    assert!(identity_error.to_string().contains("foreign table"));

    assert_eq!(
        sqlstate(
            &engine,
            "DROP SEQUENCE foreign_generated_sequence_items_serial_id_seq RESTRICT"
        ),
        "2BP01"
    );
    engine
        .sql(
            "DROP SEQUENCE foreign_generated_sequence_items_serial_id_seq CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT pg_get_serial_sequence('foreign_generated_sequence_items', 'serial_id') IS NULL AND to_regclass('foreign_generated_sequence_items') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    engine
        .sql("ALTER SEQUENCE foreign_manual_sequence OWNED BY NONE", &[])
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT pg_get_serial_sequence('foreign_generated_sequence_items', 'manual_id') IS NULL AS v"
        ),
        Value::Bool(true)
    );
    engine
        .sql("DROP FOREIGN TABLE foreign_generated_sequence_items", &[])
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('foreign_generated_sequence_items_identity_id_seq') IS NULL AND to_regclass('foreign_manual_sequence') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
}

#[test]
fn foreign_owned_sequence_drop_is_atomic_and_durable() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("foreign-owned-sequence.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SERVER durable_foreign_owned_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
            "CREATE FOREIGN TABLE durable_foreign_owned_items (serial_id serial, identity_id bigint GENERATED BY DEFAULT AS IDENTITY) SERVER durable_foreign_owned_server",
            "CREATE TABLE durable_foreign_owned_consumer (value integer DEFAULT nextval('durable_foreign_owned_items_serial_id_seq'))",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FOREIGN TABLE durable_foreign_owned_items RESTRICT"
            ),
            "2BP01"
        );
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                "DROP FOREIGN TABLE durable_foreign_owned_items CASCADE",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT to_regclass('durable_foreign_owned_items') IS NULL AND to_regclass('durable_foreign_owned_items_serial_id_seq') IS NULL AND to_regclass('durable_foreign_owned_items_identity_id_seq') IS NULL AS v"
            ),
            Value::Bool(true)
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT pg_get_serial_sequence('durable_foreign_owned_items', 'serial_id') = 'public.durable_foreign_owned_items_serial_id_seq' AND pg_get_serial_sequence('durable_foreign_owned_items', 'identity_id') = 'public.durable_foreign_owned_items_identity_id_seq' AS v"
            ),
            Value::Bool(true)
        );
    }
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "DROP FOREIGN TABLE durable_foreign_owned_items CASCADE",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT to_regclass('durable_foreign_owned_items_serial_id_seq') IS NULL AND to_regclass('durable_foreign_owned_items_identity_id_seq') IS NULL AND (SELECT NOT atthasdef FROM pg_catalog.pg_attribute AS attribute_row JOIN pg_catalog.pg_class AS relation_row ON relation_row.oid = attribute_row.attrelid WHERE relation_row.relname = 'durable_foreign_owned_consumer' AND attribute_row.attname = 'value') AS v"
            ),
            Value::Bool(true)
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT to_regclass('durable_foreign_owned_items') IS NULL AND to_regclass('durable_foreign_owned_items_serial_id_seq') IS NULL AND to_regclass('durable_foreign_owned_items_identity_id_seq') IS NULL AS v"
        ),
        Value::Bool(true)
    );
}

#[test]
fn foreign_table_owner_transfer_moves_owned_sequences_atomically() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE ROLE foreign_generated_sequence_owner; CREATE SERVER foreign_owner_sequence_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory'); CREATE FOREIGN TABLE foreign_owner_sequence_items (serial_id serial, identity_id bigint GENERATED BY DEFAULT AS IDENTITY) SERVER foreign_owner_sequence_server; ALTER FOREIGN TABLE foreign_owner_sequence_items OWNER TO foreign_generated_sequence_owner",
            &[],
        )
        .unwrap();
    let owner_count = |owner: &str| {
        scalar(
            &engine,
            &format!(
                "SELECT count(*) AS v FROM pg_catalog.pg_class AS relation_row JOIN pg_catalog.pg_roles AS role_row ON role_row.oid = relation_row.relowner WHERE relation_row.relname IN ('foreign_owner_sequence_items', 'foreign_owner_sequence_items_serial_id_seq', 'foreign_owner_sequence_items_identity_id_seq') AND role_row.rolname = '{owner}'"
            ),
        )
    };
    assert_eq!(
        owner_count("foreign_generated_sequence_owner"),
        Value::Int(3)
    );
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER FOREIGN TABLE foreign_owner_sequence_items OWNER TO CURRENT_USER",
            &[],
        )
        .unwrap();
    assert_eq!(owner_count("uqa"), Value::Int(3));
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        owner_count("foreign_generated_sequence_owner"),
        Value::Int(3)
    );
}

#[test]
fn legacy_foreign_generated_sequences_are_migrated_once() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("legacy-foreign-generated-sequences.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE SERVER legacy_foreign_generated_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory'); CREATE FOREIGN TABLE legacy_foreign_generated_items (serial_id serial, identity_id bigint GENERATED ALWAYS AS IDENTITY) SERVER legacy_foreign_generated_server",
                &[],
            )
            .unwrap();
    }
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let mut table = catalog
        .load_foreign_tables()
        .unwrap()
        .into_iter()
        .find(|table| table.relation.name == "legacy_foreign_generated_items")
        .unwrap();
    let schema: serde_json::Value = serde_json::from_str(&table.columns_json).unwrap();
    let mut columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_value(schema["columns"].clone()).unwrap();
    for column in &mut columns {
        column.object_id = None;
        column.default = None;
        let provenance = column.auto_increment.as_mut().unwrap();
        provenance.sequence = None;
        provenance.owner = None;
    }
    table.columns_json = serde_json::to_string(&columns).unwrap();
    let legacy_schema = table.columns_json.clone();
    catalog.save_foreign_table(&table).unwrap();
    let mut collision = catalog
        .load_sequence_rows()
        .unwrap()
        .into_iter()
        .find(|row| row.relation.name == "legacy_foreign_generated_items_serial_id_seq")
        .unwrap();
    assert!(catalog
        .drop_sequence_row("public.legacy_foreign_generated_items_serial_id_seq")
        .unwrap());
    assert!(catalog
        .drop_sequence_row("public.legacy_foreign_generated_items_identity_id_seq")
        .unwrap());
    collision.owner = None;
    assert!(catalog.create_sequence_row(&collision).unwrap());
    catalog.set_metadata("sql_rules_json", "{").unwrap();
    drop(catalog);

    assert!(Engine::open(&database).is_err());
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let table = catalog
        .load_foreign_tables()
        .unwrap()
        .into_iter()
        .find(|table| table.relation.name == "legacy_foreign_generated_items")
        .unwrap();
    assert_eq!(table.columns_json, legacy_schema);
    let sequence_names = catalog
        .load_sequence_rows()
        .unwrap()
        .into_iter()
        .map(|row| row.relation.name)
        .collect::<Vec<_>>();
    assert_eq!(
        sequence_names,
        vec!["legacy_foreign_generated_items_serial_id_seq"]
    );
    catalog.set_metadata("sql_rules_json", "[]").unwrap();
    drop(catalog);

    let migrated = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &migrated,
            "SELECT pg_get_serial_sequence('legacy_foreign_generated_items', 'serial_id') = 'public.legacy_foreign_generated_items_serial_id_seq1' AND pg_get_serial_sequence('legacy_foreign_generated_items', 'identity_id') = 'public.legacy_foreign_generated_items_identity_id_seq' AND to_regclass('legacy_foreign_generated_items_serial_id_seq') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    drop(migrated);
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT pg_get_serial_sequence('legacy_foreign_generated_items', 'serial_id') IS NOT NULL AND pg_get_serial_sequence('legacy_foreign_generated_items', 'identity_id') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
}
