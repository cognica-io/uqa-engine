//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `USAGE` on declared types as `PostgreSQL` 18.4 requires it: `BuildDescForRelation` for every column of a new table, view or query-defined table, including inherited columns, `ATExecAddColumn` and `ATPrepAlterColumnType`, `DefineDomain` for the base type, and `interpret_function_parameter_list` and `compute_return_type` for each argument and the result. An array type defers to its element type, a domain governs itself, and each check keeps its place among the command's other checks.

use uqa_engine::Engine;

fn denied(engine: &Engine, sql: &str, sqlstate: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
}

const DENIED: &str = "permission denied for type usage_types.feeling";

/// A role with `CREATE` on `public` and `USAGE` on the schema of an enum type whose `USAGE` was revoked from PUBLIC, owning tables that use the type.
fn fixture() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE usage_user",
        "CREATE SCHEMA usage_types",
        "CREATE TYPE usage_types.feeling AS ENUM ('ok', 'meh')",
        "CREATE DOMAIN usage_types.calm AS usage_types.feeling CHECK (VALUE <> 'meh')",
        "GRANT USAGE ON SCHEMA usage_types TO usage_user",
        "GRANT CREATE ON SCHEMA public TO usage_user",
        "REVOKE USAGE ON TYPE usage_types.feeling FROM PUBLIC",
        "CREATE TABLE usage_parent (m usage_types.feeling)",
        "ALTER TABLE usage_parent OWNER TO usage_user",
        "CREATE TABLE usage_partitioned (m usage_types.feeling, k integer) PARTITION BY LIST (k)",
        "ALTER TABLE usage_partitioned OWNER TO usage_user",
        "CREATE TABLE usage_target (a integer)",
        "ALTER TABLE usage_target OWNER TO usage_user",
        "SET ROLE usage_user",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    engine
}

#[test]
fn declarations_require_usage_on_the_governing_type() {
    let engine = fixture();
    for sql in [
        "CREATE TABLE usage_table (m usage_types.feeling)",
        "CREATE TABLE usage_array (m usage_types.feeling[])",
        "CREATE TEMP TABLE usage_temporary (m usage_types.feeling)",
        "CREATE TABLE usage_child (extra integer) INHERITS (usage_parent)",
        "CREATE TABLE usage_partition PARTITION OF usage_partitioned FOR VALUES IN (1)",
        "CREATE FOREIGN TABLE usage_foreign (m usage_types.feeling) SERVER usage_missing_server",
        "ALTER TABLE usage_target ADD COLUMN m usage_types.feeling",
        "ALTER TABLE usage_target ALTER COLUMN a TYPE usage_types.feeling USING NULL",
        "CREATE DOMAIN usage_domain AS usage_types.feeling",
        "CREATE FUNCTION usage_argument(x usage_types.feeling) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE FUNCTION usage_array_argument(x usage_types.feeling[]) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE FUNCTION usage_result() RETURNS usage_types.feeling LANGUAGE sql AS $$ SELECT 'ok'::usage_types.feeling $$",
        "CREATE FUNCTION usage_columns() RETURNS TABLE (m usage_types.feeling) LANGUAGE sql AS $$ SELECT 'ok'::usage_types.feeling $$",
        "CREATE PROCEDURE usage_output(OUT m usage_types.feeling) LANGUAGE sql AS $$ SELECT 'ok'::usage_types.feeling $$",
        "CREATE VIEW usage_view AS SELECT 'ok'::usage_types.feeling AS m",
        "CREATE MATERIALIZED VIEW usage_materialized AS SELECT 'ok'::usage_types.feeling AS m",
        "CREATE TABLE usage_query AS SELECT 'ok'::usage_types.feeling AS m",
        "SELECT 'ok'::usage_types.feeling AS m INTO usage_selected",
    ] {
        denied(&engine, sql, "42501", DENIED);
    }
    // A cast needs no USAGE, and a domain governs itself rather than its base type.
    engine
        .sql("SELECT 'ok'::usage_types.feeling AS m", &[])
        .unwrap();
    for sql in [
        "CREATE TABLE usage_domain_column (m usage_types.calm, ms usage_types.calm[])",
        "CREATE DOMAIN usage_nested AS usage_types.calm",
        "CREATE FUNCTION usage_domain_argument(x usage_types.calm) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}

#[test]
fn usage_checks_keep_their_postgresql_place() {
    let engine = fixture();
    // heap_create_with_catalog reports a taken name only after the columns are described; IF NOT EXISTS skips before.
    denied(
        &engine,
        "CREATE TABLE usage_target (m usage_types.feeling)",
        "42501",
        DENIED,
    );
    denied(
        &engine,
        "CREATE TABLE usage_target (x usage_missing_type)",
        "42704",
        "type \"usage_missing_type\" does not exist",
    );
    denied(
        &engine,
        "CREATE TABLE usage_target (x integer)",
        "42P07",
        "relation \"usage_target\" already exists",
    );
    engine
        .sql(
            "CREATE TABLE IF NOT EXISTS usage_target (m usage_types.feeling)",
            &[],
        )
        .unwrap();
    // CheckAttributeNamesTypes checks every name before any type.
    denied(
        &engine,
        "CREATE TABLE usage_names (a void, ctid integer)",
        "42701",
        "column name \"ctid\" conflicts with a system column name",
    );
    // ATExecAddColumn checks the name before the type.
    engine
        .sql(
            "ALTER TABLE usage_target ADD COLUMN IF NOT EXISTS a usage_types.feeling",
            &[],
        )
        .unwrap();
    denied(
        &engine,
        "ALTER TABLE usage_target ADD COLUMN a usage_types.feeling",
        "42701",
        "column \"a\" of relation \"usage_target\" already exists",
    );
    // ATPrepAlterColumnType finds the column before it looks up the type.
    denied(
        &engine,
        "ALTER TABLE usage_target ALTER COLUMN missing TYPE usage_types.feeling",
        "42703",
        "column \"missing\" of relation \"usage_target\" does not exist",
    );
    // CreateFunction checks CREATE on the schema and LEAKPROOF before the argument types.
    denied(
        &engine,
        "CREATE FUNCTION usage_types.usage_in_schema(x usage_types.feeling) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "42501",
        "permission denied for schema usage_types",
    );
    denied(
        &engine,
        "CREATE FUNCTION usage_leakproof(x usage_types.feeling) RETURNS integer LANGUAGE sql LEAKPROOF AS $$ SELECT 1 $$",
        "42501",
        "only superuser can define a leakproof function",
    );
    // DefineDomain rejects a pseudo-type base with a datatype mismatch.
    denied(
        &engine,
        "CREATE DOMAIN usage_void AS void",
        "42804",
        "\"void\" is not a valid base type for a domain",
    );
}

#[test]
fn owners_members_and_superusers_have_usage() {
    let engine = fixture();
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql("CREATE TABLE usage_superuser (m usage_types.feeling)", &[])
        .unwrap();
    for sql in [
        "CREATE ROLE usage_group",
        "GRANT usage_group TO usage_user",
        "GRANT USAGE ON TYPE usage_types.feeling TO usage_group",
        "SET ROLE usage_user",
        "CREATE TABLE usage_member (m usage_types.feeling)",
        "RESET ROLE",
        "REVOKE USAGE ON TYPE usage_types.feeling FROM usage_group",
        "ALTER TYPE usage_types.feeling OWNER TO usage_user",
        "SET ROLE usage_user",
        "CREATE TABLE usage_owner (m usage_types.feeling)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}
