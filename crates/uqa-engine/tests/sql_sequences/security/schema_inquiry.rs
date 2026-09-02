//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn schema_inquiry_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE schema_inquiry_owner",
        "CREATE ROLE schema_inquiry_reader",
        "CREATE ROLE schema_inquiry_outsider",
        "CREATE ROLE schema_inquiry_member INHERIT",
        "GRANT CREATE ON DATABASE uqa TO schema_inquiry_owner",
        "SET ROLE schema_inquiry_owner",
        "CREATE SCHEMA schema_inquiry_space",
        "CREATE SCHEMA \"12345\"",
        "GRANT USAGE ON SCHEMA schema_inquiry_space TO schema_inquiry_reader",
        "GRANT CREATE ON SCHEMA schema_inquiry_space TO schema_inquiry_reader WITH GRANT OPTION",
        "GRANT USAGE ON SCHEMA \"12345\" TO schema_inquiry_reader",
        "RESET ROLE",
        "REVOKE CREATE ON DATABASE uqa FROM schema_inquiry_owner",
        "GRANT schema_inquiry_owner TO schema_inquiry_member",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn assert_inquiry(engine: &Engine, sql: &str, expected: bool) {
    assert_eq!(scalar(engine, sql), Value::Bool(expected), "{sql}");
}

#[test]
fn schema_privilege_inquiry_covers_every_name_and_oid_overload() {
    let engine = schema_inquiry_engine();
    engine.sql("SET ROLE schema_inquiry_reader", &[]).unwrap();
    for (sql, expected) in [
        ("SELECT has_schema_privilege('schema_inquiry_space', 'USAGE') AS v", true),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'USAGE WITH GRANT OPTION') AS v", false),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'CREATE') AS v", true),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'CREATE WITH GRANT OPTION') AS v", true),
        ("SELECT has_schema_privilege((SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'schema_inquiry_space'), 'USAGE') AS v", true),
        ("SELECT has_schema_privilege('schema_inquiry_reader', 'schema_inquiry_space', 'USAGE') AS v", true),
        ("SELECT has_schema_privilege('schema_inquiry_reader', (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'schema_inquiry_space'), 'CREATE WITH GRANT OPTION') AS v", true),
        ("SELECT has_schema_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'schema_inquiry_reader'), 'schema_inquiry_space', 'USAGE') AS v", true),
        ("SELECT has_schema_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'schema_inquiry_reader'), (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'schema_inquiry_space'), 'CREATE') AS v", true),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'USAGE WITH GRANT OPTION, CREATE WITH GRANT OPTION') AS v", true),
        ("SELECT has_schema_privilege('12345', 'USAGE') AS v", true),
    ] {
        assert_inquiry(&engine, sql, expected);
    }
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE schema_inquiry_member", &[]).unwrap();
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege('schema_inquiry_space', 'USAGE WITH GRANT OPTION') AS v",
        true,
    );
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege('schema_inquiry_space', 'CREATE WITH GRANT OPTION') AS v",
        true,
    );
}

#[test]
fn schema_privilege_inquiry_handles_system_and_session_namespaces() {
    let engine = schema_inquiry_engine();
    engine
        .sql("CREATE TEMP TABLE schema_inquiry_temp(id integer)", &[])
        .unwrap();
    engine.sql("SET ROLE schema_inquiry_outsider", &[]).unwrap();
    for (schema, privilege, expected) in [
        ("public", "USAGE", true),
        ("public", "CREATE", false),
        ("pg_catalog", "USAGE", true),
        ("pg_catalog", "CREATE", false),
        ("information_schema", "USAGE", true),
        ("information_schema", "CREATE", false),
        ("ag_catalog", "USAGE", false),
        ("ag_catalog", "CREATE", false),
    ] {
        assert_inquiry(
            &engine,
            &format!("SELECT has_schema_privilege('{schema}', '{privilege}') AS v"),
            expected,
        );
    }
    for (privilege, expected) in [
        ("USAGE", true),
        ("CREATE", true),
        ("USAGE WITH GRANT OPTION", false),
        ("CREATE WITH GRANT OPTION", false),
    ] {
        assert_inquiry(
            &engine,
            &format!("SELECT has_schema_privilege((SELECT nspname FROM pg_catalog.pg_namespace WHERE nspname LIKE 'pg_temp_%'), '{privilege}') AS v"),
            expected,
        );
    }
    assert_eq!(
        sqlstate(&engine, "SELECT has_schema_privilege('pg_temp', 'USAGE')"),
        "3F000"
    );
}

#[test]
fn schema_privilege_inquiry_matches_missing_null_and_error_precedence() {
    let engine = schema_inquiry_engine();
    assert_eq!(
        scalar(&engine, "SELECT pg_typeof(4294967290::oid)::text AS v"),
        Value::Str("oid".into())
    );
    engine.sql("SET ROLE schema_inquiry_reader", &[]).unwrap();
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege(4294967290::oid, 'USAGE') IS NULL AS v",
        true,
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege(4294967290::oid, 'USAGE') AS v",
        true,
    );
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege(4294967289::oid, 'schema_inquiry_space', 'USAGE') AS v",
        false,
    );
    assert_inquiry(
        &engine,
        "SELECT has_schema_privilege(4294967289::oid, 4294967290::oid, 'USAGE') IS NULL AS v",
        true,
    );
    for sql in [
        "SELECT has_schema_privilege(NULL::text, 'USAGE') IS NULL AS v",
        "SELECT has_schema_privilege(NULL::oid, 'USAGE') IS NULL AS v",
        "SELECT has_schema_privilege(NULL::name, 'schema_inquiry_space', 'USAGE') IS NULL AS v",
    ] {
        assert_inquiry(&engine, sql, true);
    }
    for (sql, expected) in [
        ("SELECT has_schema_privilege('schema_inquiry_missing', 'USAGE')", "3F000"),
        ("SELECT has_schema_privilege('schema_inquiry_missing', 'schema_inquiry_space', 'USAGE')", "42704"),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'SELECT')", "22023"),
        ("SELECT has_schema_privilege('schema_inquiry_space', 'ALL')", "22023"),
        ("SELECT has_schema_privilege('schema_inquiry_missing', 'schema_inquiry_missing', 'SELECT')", "42704"),
        ("SELECT has_schema_privilege('schema_inquiry_missing', 'SELECT')", "3F000"),
        ("SELECT has_schema_privilege(4294967290::oid, 'SELECT')", "22023"),
        ("SELECT has_schema_privilege(4294967289::oid, 'schema_inquiry_space', 'SELECT')", "22023"),
        ("SELECT has_schema_privilege(4294967289::oid, 'schema_inquiry_missing', 'USAGE')", "3F000"),
    ] {
        assert_eq!(sqlstate(&engine, sql), expected, "{sql}");
    }
}

#[test]
fn schema_privilege_inquiry_exposes_postgresql_catalog_identities() {
    let engine = Engine::new();
    for (oid, identity, source) in [
        (
            2268,
            "has_schema_privilege(name,text,text)",
            "has_schema_privilege_name_name",
        ),
        (
            2269,
            "has_schema_privilege(name,oid,text)",
            "has_schema_privilege_name_id",
        ),
        (
            2270,
            "has_schema_privilege(oid,text,text)",
            "has_schema_privilege_id_name",
        ),
        (
            2271,
            "has_schema_privilege(oid,oid,text)",
            "has_schema_privilege_id_id",
        ),
        (
            2272,
            "has_schema_privilege(text,text)",
            "has_schema_privilege_name",
        ),
        (
            2273,
            "has_schema_privilege(oid,text)",
            "has_schema_privilege_id",
        ),
    ] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT oid::regprocedure::text AS v FROM pg_catalog.pg_proc WHERE oid = {oid}"
                ),
            ),
            Value::Str(identity.into())
        );
        assert_eq!(
            scalar(
                &engine,
                &format!("SELECT prosrc AS v FROM pg_catalog.pg_proc WHERE oid = {oid}"),
            ),
            Value::Str(source.into())
        );
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'has_schema_privilege' AND proisstrict AND provolatile = 's' AND proparallel = 's' AND NOT proleakproof AND prorettype = 16",
        ),
        Value::Int(6)
    );
}
