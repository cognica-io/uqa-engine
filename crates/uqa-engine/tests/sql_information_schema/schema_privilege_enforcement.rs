//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn execute(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn schema_create_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE schema_create_owner",
        "CREATE ROLE schema_create_worker",
        "CREATE ROLE schema_create_group",
        "CREATE ROLE schema_create_leaf INHERIT",
        "GRANT CREATE ON DATABASE uqa TO schema_create_owner",
        "SET ROLE schema_create_owner",
        "CREATE SCHEMA schema_create_space",
        "CREATE TABLE schema_create_space.existing_table(id integer)",
        "CREATE TABLE schema_create_space.index_target(id integer)",
        "CREATE TABLE schema_create_space.alter_target(id integer)",
        "CREATE VIEW schema_create_space.existing_view AS SELECT 1 AS id",
        "CREATE MATERIALIZED VIEW schema_create_space.existing_matview AS SELECT 1 AS id WITH NO DATA",
        "CREATE FUNCTION schema_create_space.existing_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "RESET ROLE",
        "ALTER TABLE schema_create_space.index_target OWNER TO schema_create_worker",
        "ALTER TABLE schema_create_space.alter_target OWNER TO schema_create_worker",
        "REVOKE CREATE ON DATABASE uqa FROM schema_create_owner",
        "GRANT USAGE ON SCHEMA schema_create_space TO schema_create_worker",
        "CREATE SERVER schema_create_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
    ] {
        execute(&engine, sql);
    }
    engine
}

#[test]
fn schema_create_is_enforced_across_supported_object_creation_boundaries() {
    let engine = schema_create_engine();
    execute(&engine, "SET ROLE schema_create_worker");
    for sql in [
        "CREATE TABLE schema_create_space.denied_table(id integer)",
        "CREATE UNLOGGED TABLE schema_create_space.denied_unlogged(id integer)",
        "CREATE TABLE schema_create_space.denied_ctas AS SELECT 1 AS id",
        "SELECT 1 AS id INTO schema_create_space.denied_into",
        "CREATE VIEW schema_create_space.denied_view AS SELECT 1 AS id",
        "CREATE MATERIALIZED VIEW schema_create_space.denied_matview AS SELECT 1 AS id",
        "CREATE SEQUENCE schema_create_space.denied_sequence",
        "CREATE FOREIGN TABLE schema_create_space.denied_foreign(id integer) SERVER schema_create_memory",
        "CREATE FUNCTION schema_create_space.denied_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "CREATE PROCEDURE schema_create_space.denied_procedure() LANGUAGE sql AS 'SELECT 1'",
        "CREATE INDEX denied_index ON schema_create_space.index_target USING missing_method(id)",
        "ALTER TABLE schema_create_space.alter_target ADD UNIQUE (missing_column)",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42501", "{sql}");
    }
    for sql in [
        "CREATE TABLE schema_create_space.missing_source_ctas AS SELECT * FROM schema_create_space.missing_source",
        "CREATE TABLE schema_create_space.existing_table AS SELECT * FROM schema_create_space.missing_source",
        "CREATE TABLE IF NOT EXISTS schema_create_space.existing_table AS SELECT * FROM schema_create_space.missing_source",
        "SELECT * INTO schema_create_space.missing_source_into FROM schema_create_space.missing_source",
        "CREATE VIEW schema_create_space.missing_source_view AS SELECT * FROM schema_create_space.missing_source",
        "CREATE OR REPLACE VIEW schema_create_space.existing_view AS SELECT * FROM schema_create_space.missing_source",
        "CREATE MATERIALIZED VIEW schema_create_space.missing_source_matview AS SELECT * FROM schema_create_space.missing_source",
        "CREATE MATERIALIZED VIEW IF NOT EXISTS schema_create_space.existing_table AS SELECT * FROM schema_create_space.missing_source",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42P01", "{sql}");
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_space.too_many_names(a, b) AS SELECT 1",
        ),
        "42601",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE MATERIALIZED VIEW schema_create_space.too_many_matview_names(a, b) AS SELECT 1",
        ),
        "42601",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE schema_create_space.alter_target ADD COLUMN id integer UNIQUE",
        ),
        "42701",
        "column collision validation precedes the implicit-index privilege check",
    );
    for sql in [
        "CREATE TABLE IF NOT EXISTS schema_create_space.existing_table(id integer)",
        "CREATE VIEW schema_create_space.existing_view AS SELECT 1 AS id",
        "CREATE OR REPLACE VIEW schema_create_space.existing_view AS SELECT 2 AS id",
        "CREATE OR REPLACE FUNCTION schema_create_space.existing_function() RETURNS integer LANGUAGE sql AS 'SELECT 2'",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42501", "{sql}");
    }
    execute(
        &engine,
        "CREATE TABLE IF NOT EXISTS schema_create_space.existing_table AS SELECT 1 AS id",
    );
    execute(
        &engine,
        "CREATE MATERIALIZED VIEW IF NOT EXISTS schema_create_space.existing_table AS SELECT 1 AS id",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_space.existing_table AS SELECT 1 AS id",
        ),
        "42P07",
        "a plain CTAS collision precedes the schema privilege check",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_missing.denied_table(id integer)",
        ),
        "3F000",
    );
    assert_eq!(
        sqlstate(&engine, "CREATE TABLE pg_catalog.denied_table(id integer)"),
        "42501",
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn qualified_create_needs_create_while_search_path_and_indexes_also_need_usage() {
    let engine = schema_create_engine();
    execute(
        &engine,
        "REVOKE USAGE ON SCHEMA schema_create_space FROM schema_create_worker",
    );
    execute(
        &engine,
        "GRANT CREATE ON SCHEMA schema_create_space TO schema_create_worker",
    );
    execute(&engine, "SET ROLE schema_create_worker");
    for sql in [
        "CREATE TABLE schema_create_space.granted_table(id integer)",
        "CREATE TABLE schema_create_space.granted_ctas AS SELECT 1 AS id",
        "CREATE VIEW schema_create_space.granted_view AS SELECT 1 AS id",
        "CREATE MATERIALIZED VIEW schema_create_space.granted_matview AS SELECT 1 AS id WITH NO DATA",
        "CREATE SEQUENCE schema_create_space.granted_sequence",
        "CREATE FOREIGN TABLE schema_create_space.granted_foreign(id integer) SERVER schema_create_memory",
        "CREATE FUNCTION schema_create_space.granted_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "CREATE PROCEDURE schema_create_space.granted_procedure() LANGUAGE sql AS 'SELECT 1'",
    ] {
        execute(&engine, sql);
    }
    execute(&engine, "SET search_path = schema_create_space");
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE unqualified_without_usage(id integer)"
        ),
        "3F000",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE INDEX index_without_usage ON schema_create_space.granted_table(id)",
        ),
        "42501",
    );
    execute(&engine, "RESET ROLE");
    execute(
        &engine,
        "GRANT USAGE ON SCHEMA schema_create_space TO schema_create_worker",
    );
    execute(&engine, "SET ROLE schema_create_worker");
    execute(&engine, "SET search_path = schema_create_space");
    execute(&engine, "CREATE TABLE unqualified_with_usage(id integer)");
    execute(
        &engine,
        "CREATE INDEX granted_table_id_idx ON schema_create_space.granted_table(id)",
    );
    execute(
        &engine,
        "ALTER TABLE schema_create_space.granted_table ADD UNIQUE (id)",
    );
    execute(
        &engine,
        "ALTER TABLE schema_create_space.granted_table ADD COLUMN value integer UNIQUE",
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn schema_create_inheritance_revocation_and_inferred_temporary_views_are_live() {
    let engine = schema_create_engine();
    for sql in [
        "GRANT USAGE, CREATE ON SCHEMA schema_create_space TO schema_create_group",
        "GRANT schema_create_group TO schema_create_leaf",
        "SET ROLE schema_create_leaf",
        "CREATE TABLE schema_create_space.inherited_table(id integer)",
        "CREATE FUNCTION schema_create_space.inherited_function() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "RESET ROLE",
        "REVOKE CREATE ON SCHEMA schema_create_space FROM schema_create_group",
        "SET ROLE schema_create_leaf",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_space.revoked_table(id integer)",
        ),
        "42501",
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE schema_create_worker");
    execute(
        &engine,
        "CREATE TEMP TABLE schema_create_temp_source(id integer)",
    );
    execute(
        &engine,
        "CREATE VIEW schema_create_temp_view AS SELECT * FROM schema_create_temp_source",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE VIEW schema_create_space.qualified_temp_view AS SELECT * FROM schema_create_temp_source",
        ),
        "42501",
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn schema_create_enforcement_follows_transaction_and_savepoint_acl_state() {
    let engine = schema_create_engine();
    for sql in [
        "BEGIN",
        "GRANT CREATE ON SCHEMA schema_create_space TO schema_create_worker",
        "SAVEPOINT schema_create_before_revoke",
        "REVOKE CREATE ON SCHEMA schema_create_space FROM schema_create_worker",
        "SET ROLE schema_create_worker",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_space.savepoint_denied(id integer)",
        ),
        "42501",
    );
    for sql in [
        "ROLLBACK TO SAVEPOINT schema_create_before_revoke",
        "SET ROLE schema_create_worker",
        "CREATE TABLE schema_create_space.savepoint_restored(id integer)",
        "RESET ROLE",
        "ROLLBACK",
        "SET ROLE schema_create_worker",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE schema_create_space.transaction_rolled_back(id integer)",
        ),
        "42501",
    );
    execute(&engine, "RESET ROLE");
    let count = engine
        .sql(
            "SELECT count(*) AS v FROM information_schema.tables WHERE table_schema = 'schema_create_space' AND table_name = 'savepoint_restored'",
            &[],
        )
        .unwrap();
    assert_eq!(count.rows[0]["v"], Value::Int(0));
}
