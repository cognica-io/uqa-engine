//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn scalar(engine: &Engine, sql: &str) -> Value {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .rows[0]["v"]
        .clone()
}

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn assert_inquiry(engine: &Engine, sql: &str, expected: bool) {
    assert_eq!(scalar(engine, sql), Value::Bool(expected), "{sql}");
}

fn database_privilege_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE database_privilege_reader",
        "CREATE ROLE database_privilege_outsider",
        "CREATE ROLE database_privilege_member INHERIT",
        "CREATE ROLE database_privilege_delegate",
        "GRANT uqa TO database_privilege_member",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn assert_database_inquiry_missing_and_error_boundary(engine: &Engine) {
    assert_eq!(
        scalar(engine, "SELECT pg_typeof(4294967290::oid)::text AS v"),
        Value::Str("oid".into())
    );
    assert_inquiry(
        engine,
        "SELECT has_database_privilege(4294967290::oid, 'CONNECT') AS v",
        true,
    );
    assert_inquiry(
        engine,
        "SELECT has_database_privilege(4294967289::oid, 'uqa', 'CONNECT') AS v",
        false,
    );
    assert_inquiry(
        engine,
        "SELECT has_database_privilege(4294967289::oid, 4294967290::oid, 'CONNECT') IS NULL AS v",
        true,
    );
    for sql in [
        "SELECT has_database_privilege(NULL::text, 'CONNECT') IS NULL AS v",
        "SELECT has_database_privilege(NULL::oid, 'CONNECT') IS NULL AS v",
        "SELECT has_database_privilege(NULL::name, 'uqa', 'CONNECT') IS NULL AS v",
    ] {
        assert_inquiry(engine, sql, true);
    }
    for (sql, expected) in [
        (
            "SELECT has_database_privilege('missing_database', 'CONNECT')",
            "3D000",
        ),
        (
            "SELECT has_database_privilege('missing_role', 'uqa', 'CONNECT')",
            "42704",
        ),
        ("SELECT has_database_privilege('uqa', 'SELECT')", "22023"),
        ("SELECT has_database_privilege('uqa', 'ALL')", "22023"),
        (
            "SELECT has_database_privilege('missing_role', 'missing_database', 'SELECT')",
            "42704",
        ),
        (
            "SELECT has_database_privilege('missing_database', 'SELECT')",
            "3D000",
        ),
        (
            "SELECT has_database_privilege(4294967290::oid, 'SELECT')",
            "22023",
        ),
        (
            "SELECT has_database_privilege(4294967289::oid, 'uqa', 'SELECT')",
            "22023",
        ),
        (
            "SELECT has_database_privilege(4294967289::oid, 'missing_database', 'CONNECT')",
            "3D000",
        ),
    ] {
        assert_eq!(sqlstate(engine, sql), expected, "{sql}");
    }
}

#[test]
fn database_privilege_inquiry_covers_defaults_overloads_and_errors() {
    let engine = database_privilege_engine();
    engine
        .sql("SET ROLE database_privilege_outsider", &[])
        .unwrap();
    for (privilege, expected) in [
        ("CONNECT", true),
        ("CREATE", false),
        ("TEMP", true),
        ("TEMPORARY", true),
        ("CONNECT WITH GRANT OPTION", false),
        ("TEMPORARY WITH GRANT OPTION", false),
    ] {
        assert_inquiry(
            &engine,
            &format!("SELECT has_database_privilege('uqa', '{privilege}') AS v"),
            expected,
        );
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    for sql in [
        "GRANT CREATE ON DATABASE uqa TO database_privilege_reader WITH GRANT OPTION",
        "GRANT TEMPORARY ON DATABASE uqa TO database_privilege_reader",
        "REVOKE CONNECT, TEMPORARY ON DATABASE uqa FROM PUBLIC",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    engine
        .sql("SET ROLE database_privilege_reader", &[])
        .unwrap();
    for (sql, expected) in [
        ("SELECT has_database_privilege('uqa', 'CONNECT') AS v", false),
        ("SELECT has_database_privilege('uqa', 'CREATE') AS v", true),
        ("SELECT has_database_privilege((SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa'), 'TEMP') AS v", true),
        ("SELECT has_database_privilege('database_privilege_reader', 'uqa', 'CREATE') AS v", true),
        ("SELECT has_database_privilege('database_privilege_reader', (SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa'), 'CREATE WITH GRANT OPTION') AS v", true),
        ("SELECT has_database_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'database_privilege_reader'), 'uqa', 'TEMPORARY') AS v", true),
        ("SELECT has_database_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'database_privilege_reader'), (SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa'), 'CONNECT') AS v", false),
        ("SELECT has_database_privilege('uqa', 'CONNECT, CREATE WITH GRANT OPTION') AS v", true),
    ] {
        assert_inquiry(&engine, sql, expected);
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_database_inquiry_missing_and_error_boundary(&engine);
}

#[test]
fn database_acl_grant_chains_catalog_and_owner_privileges_match_postgresql() {
    let engine = database_privilege_engine();
    for (sql, expected) in [
        (
            "GRANT SELECT ON DATABASE missing_database TO missing_role",
            "3D000",
        ),
        ("GRANT SELECT ON DATABASE uqa TO missing_role", "42704"),
        ("GRANT SELECT ON DATABASE uqa TO PUBLIC", "0LP01"),
    ] {
        assert_eq!(sqlstate(&engine, sql), expected, "{sql}");
    }
    assert_eq!(
        sqlstate(
            &engine,
            "GRANT CONNECT ON DATABASE uqa TO database_privilege_reader GRANTED BY database_privilege_reader",
        ),
        "0A000"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT coalesce(datacl::text, 'NULL') AS v FROM pg_catalog.pg_database WHERE datname = 'uqa'",
        ),
        Value::Str("NULL".into())
    );
    for sql in [
        "GRANT CREATE ON DATABASE uqa TO database_privilege_reader WITH GRANT OPTION GRANTED BY CURRENT_USER",
        "GRANT TEMP ON DATABASE uqa TO database_privilege_reader",
        "REVOKE CONNECT, TEMPORARY ON DATABASE uqa FROM PUBLIC",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT datdba::regrole::text || '|' || datacl::text AS v FROM pg_catalog.pg_database WHERE datname = 'uqa'",
        ),
        Value::Str("uqa|{uqa=CTc/uqa,database_privilege_reader=C*T/uqa}".into())
    );
    engine
        .sql("SET ROLE database_privilege_reader", &[])
        .unwrap();
    engine
        .sql(
            "GRANT CREATE ON DATABASE uqa TO database_privilege_delegate",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa FROM database_privilege_reader RESTRICT",
        ),
        "2BP01"
    );
    engine
        .sql(
            "REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa FROM database_privilege_reader CASCADE",
            &[],
        )
        .unwrap();
    assert_inquiry(
        &engine,
        "SELECT has_database_privilege('database_privilege_delegate', 'uqa', 'CREATE') AS v",
        false,
    );
    engine
        .sql("REVOKE ALL PRIVILEGES ON DATABASE uqa FROM uqa", &[])
        .unwrap();
    engine
        .sql("SET ROLE database_privilege_member", &[])
        .unwrap();
    for privilege in [
        "CONNECT WITH GRANT OPTION",
        "CREATE WITH GRANT OPTION",
        "TEMPORARY WITH GRANT OPTION",
    ] {
        assert_inquiry(
            &engine,
            &format!("SELECT has_database_privilege('uqa', '{privilege}') AS v"),
            true,
        );
    }
}

#[test]
fn database_acl_is_transactional_cross_engine_durable_and_role_dependent() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("database-privileges.sqlite");
    let writer = Engine::open(&database).unwrap();
    let observer = Engine::open(&database).unwrap();
    writer
        .sql("CREATE ROLE database_privilege_persistent", &[])
        .unwrap();
    writer.sql("BEGIN", &[]).unwrap();
    writer
        .sql(
            "GRANT CREATE ON DATABASE uqa TO database_privilege_persistent",
            &[],
        )
        .unwrap();
    writer.sql("ROLLBACK", &[]).unwrap();
    assert_inquiry(
        &writer,
        "SELECT has_database_privilege('database_privilege_persistent', 'uqa', 'CREATE') AS v",
        false,
    );
    writer.sql("BEGIN", &[]).unwrap();
    writer
        .sql(
            "GRANT CREATE ON DATABASE uqa TO database_privilege_persistent",
            &[],
        )
        .unwrap();
    writer.sql("SAVEPOINT database_acl", &[]).unwrap();
    writer
        .sql(
            "REVOKE CREATE ON DATABASE uqa FROM database_privilege_persistent",
            &[],
        )
        .unwrap();
    assert_inquiry(
        &writer,
        "SELECT has_database_privilege('database_privilege_persistent', 'uqa', 'CREATE') AS v",
        false,
    );
    writer
        .sql("ROLLBACK TO SAVEPOINT database_acl", &[])
        .unwrap();
    writer.sql("COMMIT", &[]).unwrap();
    assert_inquiry(
        &observer,
        "SELECT has_database_privilege('database_privilege_persistent', 'uqa', 'CREATE') AS v",
        true,
    );
    drop(observer);
    drop(writer);
    let reopened = Engine::open(&database).unwrap();
    assert_inquiry(
        &reopened,
        "SELECT has_database_privilege('database_privilege_persistent', 'uqa', 'CREATE') AS v",
        true,
    );
    assert_eq!(
        sqlstate(&reopened, "DROP ROLE database_privilege_persistent"),
        "2BP01"
    );
    reopened
        .sql(
            "REVOKE CREATE ON DATABASE uqa FROM database_privilege_persistent",
            &[],
        )
        .unwrap();
    reopened
        .sql("DROP ROLE database_privilege_persistent", &[])
        .unwrap();
}

#[test]
fn database_privilege_inquiry_exposes_postgresql_catalog_identities() {
    let engine = Engine::new();
    for (oid, identity, source) in [
        (
            2250,
            "has_database_privilege(name,text,text)",
            "has_database_privilege_name_name",
        ),
        (
            2251,
            "has_database_privilege(name,oid,text)",
            "has_database_privilege_name_id",
        ),
        (
            2252,
            "has_database_privilege(oid,text,text)",
            "has_database_privilege_id_name",
        ),
        (
            2253,
            "has_database_privilege(oid,oid,text)",
            "has_database_privilege_id_id",
        ),
        (
            2254,
            "has_database_privilege(text,text)",
            "has_database_privilege_name",
        ),
        (
            2255,
            "has_database_privilege(oid,text)",
            "has_database_privilege_id",
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
            "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'has_database_privilege' AND proisstrict AND provolatile = 's' AND proparallel = 's' AND NOT proleakproof AND prorettype = 16",
        ),
        Value::Int(6)
    );
}
