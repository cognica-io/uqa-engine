//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn create_schema_security_roles(engine: &Engine) {
    for sql in [
        "CREATE ROLE schema_acl_owner",
        "CREATE ROLE schema_sequence_owner",
        "CREATE ROLE schema_sequence_new_owner",
        "CREATE ROLE schema_sequence_outsider",
        "GRANT CREATE ON DATABASE uqa TO schema_acl_owner",
        "SET ROLE schema_acl_owner",
        "CREATE SCHEMA sequence_source",
        "CREATE SCHEMA sequence_target",
        "RESET ROLE",
        "REVOKE CREATE ON DATABASE uqa FROM schema_acl_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn sequence_schema_privileges_cover_create_and_access() {
    let engine = Engine::new();
    create_schema_security_roles(&engine);
    verify_schema_create_privilege(&engine);
    verify_sequence_schema_usage(&engine);
    assert_eq!(
        scalar(
            &engine,
            "SELECT nspacl::text AS v FROM pg_catalog.pg_namespace WHERE nspname = 'sequence_source'",
        ),
        Value::Str("{schema_acl_owner=UC/schema_acl_owner,schema_sequence_owner=UC/schema_acl_owner,schema_sequence_outsider=U/schema_acl_owner}".into())
    );
}

fn verify_schema_create_privilege(engine: &Engine) {
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(engine, "CREATE SEQUENCE sequence_source.ids"),
        "42501"
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM information_schema.schemata WHERE schema_name = 'sequence_source'",
        ),
        Value::Int(0)
    );
    assert_eq!(
        sqlstate(
            engine,
            "CREATE SEQUENCE sequence_source.invalid_ids INCREMENT 0",
        ),
        "22023"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql(
            "GRANT CREATE ON SCHEMA sequence_source TO schema_sequence_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    engine
        .sql("CREATE SEQUENCE sequence_source.ids", &[])
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT schema_owner AS v FROM information_schema.schemata WHERE schema_name = 'sequence_source'",
        ),
        Value::Str("schema_acl_owner".into())
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "REVOKE CREATE ON SCHEMA sequence_source FROM schema_sequence_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "CREATE SEQUENCE IF NOT EXISTS sequence_source.ids",
        ),
        "42501",
        "CREATE permission is checked before IF NOT EXISTS collision handling only after it is revoked",
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn verify_sequence_schema_usage(engine: &Engine) {
    engine
        .sql(
            "GRANT USAGE, CREATE ON SCHEMA sequence_source TO schema_sequence_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE, SELECT, UPDATE ON SEQUENCE sequence_source.ids TO schema_sequence_outsider",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql("SET ROLE schema_sequence_outsider", &[])
        .unwrap();
    assert_eq!(
        sqlstate(engine, "SELECT nextval('sequence_source.ids')"),
        "42501"
    );
    assert_eq!(
        sqlstate(engine, "SELECT * FROM sequence_source.ids"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA sequence_source TO schema_sequence_outsider",
            &[],
        )
        .unwrap();
    engine
        .sql("SET ROLE schema_sequence_outsider", &[])
        .unwrap();
    assert_eq!(
        scalar(engine, "SELECT nextval('sequence_source.ids') AS v"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(engine, "SELECT last_value AS v FROM sequence_source.ids"),
        Value::Int(1)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

#[test]
fn sequence_schema_privileges_cover_owner_and_namespace_changes() {
    let engine = Engine::new();
    create_schema_security_roles(&engine);
    for sql in [
        "GRANT USAGE, CREATE ON SCHEMA sequence_source TO schema_sequence_owner",
        "SET ROLE schema_sequence_owner",
        "CREATE SEQUENCE sequence_source.ids",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
        .sql(
            "GRANT schema_sequence_new_owner TO schema_sequence_owner WITH INHERIT FALSE, SET TRUE",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER SEQUENCE sequence_source.ids OWNER TO schema_sequence_new_owner",
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT CREATE ON SCHEMA sequence_source TO schema_sequence_new_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE schema_sequence_owner", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE sequence_source.ids OWNER TO schema_sequence_new_owner",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();

    engine
        .sql(
            "GRANT USAGE ON SCHEMA sequence_source TO schema_sequence_new_owner",
            &[],
        )
        .unwrap();
    engine
        .sql("SET ROLE schema_sequence_new_owner", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER SEQUENCE sequence_source.ids SET SCHEMA sequence_target",
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT CREATE ON SCHEMA sequence_target TO schema_sequence_new_owner",
            &[],
        )
        .unwrap();
    engine
        .sql("SET ROLE schema_sequence_new_owner", &[])
        .unwrap();
    engine
        .sql(
            "ALTER SEQUENCE sequence_source.ids SET SCHEMA sequence_target",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();

    assert_eq!(
        scalar(
            &engine,
            "SELECT r.rolname AS v FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles r ON r.oid = n.nspowner WHERE n.nspname = 'sequence_source'",
        ),
        Value::Str("schema_acl_owner".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT nspacl::text AS v FROM pg_catalog.pg_namespace WHERE nspname = 'sequence_source'",
        ),
        Value::Str("{schema_acl_owner=UC/schema_acl_owner,schema_sequence_owner=UC/schema_acl_owner,schema_sequence_new_owner=UC/schema_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT nspacl::text AS v FROM pg_catalog.pg_namespace WHERE nspname = 'sequence_target'",
        ),
        Value::Str("{schema_acl_owner=UC/schema_acl_owner,schema_sequence_new_owner=C/schema_acl_owner}".into())
    );
}

#[test]
fn sequence_schema_name_error_precedence_matches_postgresql() {
    let engine = Engine::new();
    for sql in [
        "SELECT nextval('missing_sequence_schema.ids')",
        "SELECT * FROM missing_sequence_schema.ids",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42P01", "{sql}");
    }
    for sql in [
        "ALTER SEQUENCE missing_sequence_schema.ids CACHE 2",
        "DROP SEQUENCE missing_sequence_schema.ids",
        "GRANT USAGE ON SEQUENCE missing_sequence_schema.ids TO PUBLIC",
        "SELECT has_sequence_privilege('missing_sequence_schema.ids', 'USAGE')",
        "ALTER SEQUENCE pg_temp.missing_ids CACHE 2",
    ] {
        assert_eq!(sqlstate(&engine, sql), "3F000", "{sql}");
    }
    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS missing_sequence_schema.ids CACHE 2",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP SEQUENCE IF EXISTS missing_sequence_schema.ids", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [
            (
                "NOTICE".into(),
                "relation \"missing_sequence_schema.ids\" does not exist, skipping".into(),
            ),
            (
                "NOTICE".into(),
                "sequence \"missing_sequence_schema.ids\" does not exist, skipping".into(),
            ),
        ]
    );
    engine
        .sql(
            "CREATE TEMP TABLE allocate_temp_namespace (id integer)",
            &[],
        )
        .unwrap();
    assert_eq!(
        sqlstate(&engine, "ALTER SEQUENCE pg_temp.missing_ids CACHE 2"),
        "42P01"
    );
}

#[test]
fn schema_grant_error_precedence_and_atomicity_match_postgresql() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE schema_grant_owner",
        "CREATE ROLE schema_grant_user",
        "GRANT CREATE ON DATABASE uqa TO schema_grant_owner",
        "SET ROLE schema_grant_owner",
        "CREATE SCHEMA schema_grant_space",
        "RESET ROLE",
        "REVOKE CREATE ON DATABASE uqa FROM schema_grant_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "GRANT USAGE ON SCHEMA schema_grant_missing TO schema_grant_missing_role",
        ),
        "25006"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    for (sql, expected) in [
        (
            "GRANT SELECT ON SCHEMA schema_grant_missing TO schema_grant_missing_role",
            "3F000",
        ),
        (
            "GRANT SELECT ON SCHEMA schema_grant_space TO schema_grant_missing_role",
            "42704",
        ),
        (
            "GRANT SELECT ON SCHEMA schema_grant_space TO schema_grant_user",
            "0LP01",
        ),
        (
            "GRANT USAGE ON SCHEMA schema_grant_space TO PUBLIC WITH GRANT OPTION",
            "0LP01",
        ),
        (
            "GRANT USAGE ON SCHEMA schema_grant_space, schema_grant_missing TO schema_grant_user",
            "3F000",
        ),
    ] {
        assert_eq!(sqlstate(&engine, sql), expected, "{sql}");
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT nspacl IS NULL AS v FROM pg_catalog.pg_namespace WHERE nspname = 'schema_grant_space'",
        ),
        Value::Bool(true),
        "multi-target failure must not mutate an earlier schema",
    );
    engine.sql("SET ROLE schema_grant_user", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA schema_grant_space TO schema_grant_user",
            &[],
        )
        .unwrap();
    assert_single_warning(
        &engine,
        "no privileges were granted for \"schema_grant_space\"",
    );
}

#[test]
fn schema_checks_preserve_owned_sequence_and_noop_precedence() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE schema_order_owner",
        "CREATE ROLE schema_order_new_owner",
        "CREATE SCHEMA schema_order_source",
        "CREATE SCHEMA schema_order_target",
        "GRANT USAGE, CREATE ON SCHEMA schema_order_source TO schema_order_owner",
        "GRANT schema_order_new_owner TO schema_order_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE schema_order_owner",
        "CREATE SEQUENCE schema_order_source.ids",
        "CREATE TABLE schema_order_source.rows (id serial)",
        "RESET ROLE",
        "REVOKE CREATE ON SCHEMA schema_order_source FROM schema_order_owner",
        "SET ROLE schema_order_owner",
        "ALTER SEQUENCE schema_order_source.ids OWNER TO schema_order_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for sql in [
        "ALTER SEQUENCE schema_order_source.rows_id_seq OWNER TO schema_order_new_owner",
        "ALTER SEQUENCE schema_order_source.rows_id_seq SET SCHEMA schema_order_source",
        "ALTER SEQUENCE schema_order_source.rows_id_seq SET SCHEMA schema_order_target",
        "ALTER SEQUENCE schema_order_source.rows_id_seq SET SCHEMA schema_order_missing",
    ] {
        assert_eq!(sqlstate(&engine, sql), "0A000", "{sql}");
    }
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER SEQUENCE schema_order_source.ids OWNER TO schema_order_new_owner",
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER SEQUENCE schema_order_source.ids SET SCHEMA schema_order_source",
        ),
        "42501"
    );
}

#[test]
fn schema_acl_grant_paths_are_dependency_aware_transactional_and_durable() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("schema-sequence-security.db");
    {
        let engine = Engine::open(&database).unwrap();
        create_schema_acl_chain(&engine);
        let observer = Engine::open(&database).unwrap();
        verify_schema_acl_transaction_visibility(&engine, &observer);
        assert_eq!(sqlstate(&engine, "DROP ROLE schema_chain_owner"), "2BP01");
        assert_eq!(
            sqlstate(&engine, "DROP ROLE schema_chain_delegate"),
            "2BP01"
        );
        engine.sql("DROP ROLE schema_chain_leaf", &[]).unwrap();
    }
    let reopened = Engine::open(&database).unwrap();
    assert_schema_chain_acl(&reopened, SCHEMA_CHAIN_BASE_ACL);
}

const SCHEMA_CHAIN_BASE_ACL: &str =
    "{schema_chain_owner=UC/schema_chain_owner,schema_chain_delegate=U/schema_chain_owner}";
const SCHEMA_CHAIN_LEAF_ACL: &str = "{schema_chain_owner=UC/schema_chain_owner,schema_chain_delegate=U/schema_chain_owner,schema_chain_leaf=C/schema_chain_owner}";

fn create_schema_acl_chain(engine: &Engine) {
    for sql in [
        "CREATE ROLE schema_chain_owner",
        "CREATE ROLE schema_chain_delegate",
        "CREATE ROLE schema_chain_leaf",
        "GRANT CREATE ON DATABASE uqa TO schema_chain_owner",
        "SET ROLE schema_chain_owner",
        "CREATE SCHEMA schema_chain_space",
        "GRANT USAGE ON SCHEMA schema_chain_space TO schema_chain_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "REVOKE CREATE ON DATABASE uqa FROM schema_chain_owner",
        "SET ROLE schema_chain_delegate",
        "GRANT USAGE ON SCHEMA schema_chain_space TO schema_chain_leaf",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine.sql("SET ROLE schema_chain_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "REVOKE GRANT OPTION FOR USAGE ON SCHEMA schema_chain_space FROM schema_chain_delegate RESTRICT",
        ),
        "2BP01"
    );
    engine
        .sql(
            "REVOKE GRANT OPTION FOR USAGE ON SCHEMA schema_chain_space FROM schema_chain_delegate CASCADE",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_schema_chain_acl(engine, SCHEMA_CHAIN_BASE_ACL);
}

fn verify_schema_acl_transaction_visibility(engine: &Engine, observer: &Engine) {
    engine.sql("BEGIN", &[]).unwrap();
    grant_schema_create_to_leaf(engine);
    assert_schema_chain_acl(observer, SCHEMA_CHAIN_BASE_ACL);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_schema_chain_acl(engine, SCHEMA_CHAIN_BASE_ACL);

    grant_schema_create_to_leaf(engine);
    assert_schema_chain_acl(observer, SCHEMA_CHAIN_LEAF_ACL);
    for sql in [
        "SET ROLE schema_chain_owner",
        "REVOKE CREATE ON SCHEMA schema_chain_space FROM schema_chain_leaf",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_schema_chain_acl(observer, SCHEMA_CHAIN_BASE_ACL);
}

fn grant_schema_create_to_leaf(engine: &Engine) {
    for sql in [
        "SET ROLE schema_chain_owner",
        "GRANT CREATE ON SCHEMA schema_chain_space TO schema_chain_leaf",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

fn assert_schema_chain_acl(engine: &Engine, expected: &str) {
    assert_eq!(
        scalar(
            engine,
            "SELECT nspacl::text AS v FROM pg_catalog.pg_namespace WHERE nspname = 'schema_chain_space'",
        ),
        Value::Str(expected.into())
    );
}
