//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[path = "security/targets.rs"]
mod targets;
#[path = "security/temporary_acl.rs"]
mod temporary_acl;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .rows[0]["v"]
        .clone()
}

fn assert_single_warning(engine: &Engine, message: &str) {
    assert_eq!(
        engine.take_sql_notices(),
        [("WARNING".into(), message.into())]
    );
}

fn sequence_owner(engine: &Engine, name: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT sequenceowner AS v FROM pg_catalog.pg_sequences WHERE schemaname = 'public' AND sequencename = '{name}'"
        ),
    )
}

fn sequence_owner_engine() -> (Engine, Value, [u8; 16]) {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE sequence_owner",
        "CREATE ROLE sequence_next_owner",
        "CREATE ROLE sequence_outsider",
        "CREATE ROLE sequence_owner_member INHERIT",
        "CREATE ROLE sequence_owner_noinherit NOINHERIT",
        "SET ROLE sequence_owner",
        "CREATE SEQUENCE role_owned_ids CACHE 3",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_eq!(
        sequence_owner(&engine, "role_owned_ids"),
        Value::Str("sequence_owner".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT c.relowner AS v FROM pg_catalog.pg_class c WHERE c.relname = 'role_owned_ids'",
        ),
        scalar(
            &engine,
            "SELECT r.oid AS v FROM pg_catalog.pg_roles r WHERE r.rolname = 'sequence_owner'",
        )
    );
    let relation_oid = scalar(
        &engine,
        "SELECT c.oid AS v FROM pg_catalog.pg_class c WHERE c.relname = 'role_owned_ids'",
    );
    let definition_generation = engine
        .sequence_state("role_owned_ids")
        .unwrap()
        .expect("sequence state")
        .1
        .definition_generation;
    (engine, relation_oid, definition_generation)
}

fn assert_owner_transfer_authority(engine: &Engine, definition_generation: [u8; 16]) {
    engine.sql("SET ROLE sequence_outsider", &[]).unwrap();
    for sql in [
        "ALTER SEQUENCE role_owned_ids CACHE 2",
        "ALTER SEQUENCE role_owned_ids RENAME TO rejected_ids",
        "DROP SEQUENCE role_owned_ids",
        "ALTER SEQUENCE role_owned_ids OWNER TO missing_role",
    ] {
        assert_eq!(sqlstate(engine, sql), "42501", "{sql}");
    }
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE sequence_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "ALTER SEQUENCE role_owned_ids OWNER TO sequence_next_owner",
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT sequence_next_owner TO sequence_owner WITH INHERIT FALSE, SET TRUE",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE sequence_owner", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE role_owned_ids OWNER TO sequence_next_owner",
            &[],
        )
        .unwrap();
    assert_eq!(
        sequence_owner(engine, "role_owned_ids"),
        Value::Str("sequence_next_owner".into())
    );
    assert_eq!(
        engine
            .sequence_state("role_owned_ids")
            .unwrap()
            .expect("sequence state")
            .1
            .definition_generation,
        definition_generation,
        "owner transfer is not a sequence definition change"
    );
    assert_eq!(
        sqlstate(
            engine,
            "ALTER SEQUENCE role_owned_ids RENAME TO rejected_ids",
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn assert_owner_lifecycle_and_membership(engine: &Engine, relation_oid: &Value) {
    engine.sql("SET ROLE sequence_next_owner", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE role_owned_ids RENAME TO renamed_ids", &[])
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql("ALTER TABLE renamed_ids OWNER TO sequence_owner", &[])
        .unwrap();
    assert_eq!(
        sequence_owner(engine, "renamed_ids"),
        Value::Str("sequence_owner".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT c.oid AS v FROM pg_catalog.pg_class c WHERE c.relname = 'renamed_ids'",
        ),
        relation_oid.clone(),
        "owner transfer and rename preserve relation identity"
    );
    engine
        .sql(
            "GRANT sequence_owner TO sequence_owner_member, sequence_owner_noinherit",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE sequence_owner_member", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE renamed_ids CACHE 2", &[])
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql("SET ROLE sequence_owner_noinherit", &[])
        .unwrap();
    assert_eq!(
        sqlstate(engine, "ALTER SEQUENCE renamed_ids CACHE 1"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn assert_owner_dependency_and_resolution_errors(engine: &Engine) {
    engine
        .sql("CREATE TABLE serial_rows (id SERIAL)", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "ALTER SEQUENCE serial_rows_id_seq OWNER TO sequence_owner",
        ),
        "0A000"
    );
    assert_eq!(
        sqlstate(engine, "ALTER SEQUENCE renamed_ids OWNER TO PUBLIC"),
        "42704"
    );
    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "ALTER SEQUENCE IF EXISTS missing_role_ids OWNER TO missing_role",
        ),
        "25006"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS missing_role_ids OWNER TO missing_role",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS missing_public_ids OWNER TO PUBLIC",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [
            (
                "NOTICE".into(),
                "relation \"missing_role_ids\" does not exist, skipping".into()
            ),
            (
                "NOTICE".into(),
                "relation \"missing_public_ids\" does not exist, skipping".into()
            )
        ]
    );
    assert_eq!(sqlstate(engine, "DROP ROLE sequence_owner"), "2BP01");
}

#[test]
fn sequence_role_owner_controls_administration_and_catalogs() {
    let (engine, relation_oid, definition_generation) = sequence_owner_engine();
    assert_owner_transfer_authority(&engine, definition_generation);
    assert_owner_lifecycle_and_membership(&engine, &relation_oid);
    assert_owner_dependency_and_resolution_errors(&engine);
}

fn assert_durable_owner_transactions(engine: &Engine) {
    for sql in [
        "CREATE ROLE durable_sequence_owner",
        "CREATE ROLE durable_sequence_next_owner",
        "GRANT durable_sequence_next_owner TO durable_sequence_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE durable_sequence_owner",
        "CREATE SEQUENCE durable_role_ids CACHE 4",
        "BEGIN",
        "ALTER SEQUENCE durable_role_ids OWNER TO durable_sequence_next_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        sequence_owner(engine, "durable_role_ids"),
        Value::Str("durable_sequence_next_owner".into())
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        sequence_owner(engine, "durable_role_ids"),
        Value::Str("durable_sequence_owner".into())
    );
    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SAVEPOINT owner_change", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE durable_role_ids OWNER TO durable_sequence_next_owner",
            &[],
        )
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT owner_change", &[])
        .unwrap();
    assert_eq!(
        sequence_owner(engine, "durable_role_ids"),
        Value::Str("durable_sequence_owner".into())
    );
    engine
        .sql(
            "ALTER SEQUENCE durable_role_ids OWNER TO durable_sequence_next_owner",
            &[],
        )
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        sequence_owner(engine, "durable_role_ids"),
        Value::Str("durable_sequence_next_owner".into())
    );
}

fn assert_temporary_owner_transactions(engine: &Engine) {
    engine
        .sql("CREATE TEMP SEQUENCE temporary_role_ids", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE temporary_role_ids OWNER TO durable_sequence_next_owner",
            &[],
        )
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT sequenceowner AS v FROM pg_catalog.pg_sequences WHERE sequencename = 'temporary_role_ids'",
        ),
        Value::Str("durable_sequence_owner".into())
    );
    engine
        .sql(
            "ALTER SEQUENCE temporary_role_ids OWNER TO durable_sequence_next_owner",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT sequenceowner AS v FROM pg_catalog.pg_sequences WHERE sequencename = 'temporary_role_ids'",
        ),
        Value::Str("durable_sequence_next_owner".into())
    );
}

fn assert_reopened_owner(database: &std::path::Path) {
    let reopened = Engine::open(database).unwrap();
    assert_eq!(
        sequence_owner(&reopened, "durable_role_ids"),
        Value::Str("durable_sequence_next_owner".into())
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_sequences WHERE sequencename = 'temporary_role_ids'",
        ),
        Value::Int(0)
    );
    reopened
        .sql("SET ROLE durable_sequence_next_owner", &[])
        .unwrap();
    reopened
        .sql(
            "ALTER SEQUENCE durable_role_ids RENAME TO reopened_role_ids",
            &[],
        )
        .unwrap();
    assert_eq!(
        sequence_owner(&reopened, "reopened_role_ids"),
        Value::Str("durable_sequence_next_owner".into())
    );
}

#[test]
fn sequence_role_owner_follows_transactions_savepoints_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-role-owner.db");
    {
        let engine = Engine::open(&database).unwrap();
        assert_durable_owner_transactions(&engine);
        assert_temporary_owner_transactions(&engine);
    }
    assert_reopened_owner(&database);
}

fn sequence_acl_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE sequence_acl_owner",
        "CREATE ROLE sequence_acl_usage",
        "CREATE ROLE sequence_acl_select",
        "CREATE ROLE sequence_acl_update",
        "CREATE ROLE sequence_acl_delegate",
        "CREATE ROLE sequence_acl_outsider",
        "CREATE ROLE sequence_acl_member",
        "SET ROLE sequence_acl_owner",
        "CREATE SEQUENCE sequence_acl_ids",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl IS NULL AS v FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_ids'",
        ),
        Value::Bool(true)
    );
    for sql in [
        "SET ROLE sequence_acl_owner",
        "GRANT USAGE ON SEQUENCE sequence_acl_ids TO sequence_acl_usage",
        "GRANT SELECT ON SEQUENCE sequence_acl_ids TO sequence_acl_select",
        "GRANT UPDATE ON SEQUENCE sequence_acl_ids TO sequence_acl_update",
        "GRANT USAGE, SELECT ON SEQUENCE sequence_acl_ids TO sequence_acl_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "GRANT sequence_acl_usage TO sequence_acl_member",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn assert_sequence_acl_function_mapping(engine: &Engine) {
    engine.sql("SET ROLE sequence_acl_usage", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT nextval('sequence_acl_ids') AS v"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(engine, "SELECT currval('sequence_acl_ids') AS v"),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(engine, "SELECT setval('sequence_acl_ids', 20)"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    assert_eq!(
        scalar(engine, "SELECT nextval('sequence_acl_ids') AS v"),
        Value::Int(2)
    );
    engine.sql("SET ROLE sequence_acl_select", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT currval('sequence_acl_ids') AS v"),
        Value::Int(2)
    );
    assert_eq!(scalar(engine, "SELECT lastval() AS v"), Value::Int(2));
    assert_eq!(
        sqlstate(engine, "SELECT nextval('sequence_acl_ids')"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE sequence_acl_update", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT setval('sequence_acl_ids', 30) AS v"),
        Value::Int(30)
    );
    assert_eq!(
        scalar(engine, "SELECT nextval('sequence_acl_ids') AS v"),
        Value::Int(31)
    );
    assert_eq!(
        sqlstate(engine, "SELECT currval('sequence_acl_ids')"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE sequence_acl_outsider", &[]).unwrap();
    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        sqlstate(engine, "SELECT nextval('sequence_acl_ids')"),
        "42501"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        sqlstate(engine, "SELECT currval('sequence_acl_ids')"),
        "42501"
    );
    assert_eq!(
        sqlstate(engine, "SELECT setval('sequence_acl_ids', 40)"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE sequence_acl_member", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT nextval('sequence_acl_ids') AS v"),
        Value::Int(32)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn assert_sequence_privilege_inquiry(engine: &Engine) {
    for (sql, expected) in [
        ("SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_ids', 'USAGE') AS v", true),
        ("SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_ids', 'USAGE, UPDATE') AS v", true),
        ("SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_ids', 'SELECT, UPDATE') AS v", false),
        ("SELECT has_sequence_privilege('sequence_acl_delegate', 'sequence_acl_ids', 'SELECT WITH GRANT OPTION') AS v", true),
        ("SELECT has_sequence_privilege('sequence_acl_outsider', 'sequence_acl_ids', 'USAGE') AS v", false),
        ("SELECT has_sequence_privilege('sequence_acl_ids', 'UPDATE') AS v", true),
    ] {
        assert_eq!(scalar(engine, sql), Value::Bool(expected), "{sql}");
    }
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_ids'), 'USAGE') AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege((SELECT oid FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_ids'), 'UPDATE') AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'sequence_acl_usage'), 'sequence_acl_ids', 'USAGE') AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'sequence_acl_usage'), (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_ids'), 'USAGE') AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege((SELECT 4294967289::oid FROM pg_catalog.pg_roles LIMIT 1), 'sequence_acl_ids', 'USAGE') AS v",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege(NULL::text, 'USAGE') IS NULL AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', (SELECT 4294967290::oid FROM pg_catalog.pg_class LIMIT 1), 'USAGE') IS NULL AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_ids', 'EXECUTE')",
        ),
        "22023"
    );
    engine
        .sql("CREATE TABLE sequence_acl_table (id integer)", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_table', 'USAGE')",
        ),
        "42809"
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_missing_role', 'sequence_acl_ids', 'USAGE')",
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', 'sequence_acl_missing', 'USAGE')",
        ),
        "42P01"
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_sequence_privilege('sequence_acl_usage', (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_table'), 'USAGE')",
        ),
        "42809"
    );
}

fn assert_sequence_privilege_catalog(engine: &Engine) {
    for (oid, source) in [
        (2181, "has_sequence_privilege_name_name"),
        (2182, "has_sequence_privilege_name_id"),
        (2183, "has_sequence_privilege_id_name"),
        (2184, "has_sequence_privilege_id_id"),
        (2185, "has_sequence_privilege_name"),
        (2186, "has_sequence_privilege_id"),
    ] {
        assert_eq!(
            scalar(
                engine,
                &format!("SELECT prosrc AS v FROM pg_catalog.pg_proc WHERE oid = {oid}"),
            ),
            Value::Str(source.into()),
            "pg_proc OID {oid}"
        );
    }
}

#[test]
fn sequence_acl_controls_value_functions_catalog_and_inquiry() {
    let engine = sequence_acl_engine();
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text AS v FROM pg_catalog.pg_class WHERE relname = 'sequence_acl_ids'",
        ),
        Value::Str(
            "{sequence_acl_owner=rwU/sequence_acl_owner,sequence_acl_usage=U/sequence_acl_owner,sequence_acl_select=r/sequence_acl_owner,sequence_acl_update=w/sequence_acl_owner,sequence_acl_delegate=r*U*/sequence_acl_owner}".into(),
        )
    );
    assert_sequence_acl_function_mapping(&engine);
    assert_sequence_privilege_inquiry(&engine);
    assert_sequence_privilege_catalog(&engine);
}

#[test]
fn sequence_owner_can_revoke_ordinary_privileges_and_transfer_acl_ownership() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE acl_self_owner",
        "CREATE ROLE acl_next_owner",
        "CREATE ROLE acl_owner_reader",
        "GRANT acl_next_owner TO acl_self_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE acl_self_owner",
        "CREATE SEQUENCE acl_owner_ids",
        "REVOKE ALL PRIVILEGES ON SEQUENCE acl_owner_ids FROM acl_self_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text AS v FROM pg_catalog.pg_class WHERE relname = 'acl_owner_ids'",
        ),
        Value::Str("{}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_self_owner', 'acl_owner_ids', 'USAGE') AS v",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_self_owner', 'acl_owner_ids', 'USAGE WITH GRANT OPTION') AS v",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(&engine, "SELECT nextval('acl_owner_ids')"),
        "42501"
    );
    engine
        .sql("ALTER SEQUENCE acl_owner_ids CACHE 2", &[])
        .unwrap();
    for sql in [
        "GRANT ALL PRIVILEGES ON SEQUENCE acl_owner_ids TO acl_self_owner",
        "GRANT USAGE ON SEQUENCE acl_owner_ids TO acl_next_owner WITH GRANT OPTION",
        "GRANT USAGE ON SEQUENCE acl_owner_ids TO acl_owner_reader",
        "ALTER SEQUENCE acl_owner_ids OWNER TO acl_next_owner",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text AS v FROM pg_catalog.pg_class WHERE relname = 'acl_owner_ids'",
        ),
        Value::Str("{acl_next_owner=rwU*/acl_next_owner,acl_owner_reader=U/acl_next_owner}".into(),)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_self_owner', 'acl_owner_ids', 'USAGE') AS v",
        ),
        Value::Bool(false)
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE acl_owner_reader"), "2BP01");
    for sql in [
        "SET ROLE acl_next_owner",
        "REVOKE USAGE ON SEQUENCE acl_owner_ids FROM acl_owner_reader",
        "RESET ROLE",
        "DROP ROLE acl_owner_reader",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn sequence_acl_grant_chains_follow_restrict_cascade_and_alternate_paths() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE acl_chain_owner",
        "CREATE ROLE acl_chain_delegate",
        "CREATE ROLE acl_chain_leaf",
        "CREATE ROLE acl_chain_tail",
        "CREATE ROLE acl_chain_outsider",
        "SET ROLE acl_chain_owner",
        "CREATE SEQUENCE acl_chain_ids",
        "GRANT USAGE ON SEQUENCE acl_chain_ids TO acl_chain_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE acl_chain_delegate",
        "GRANT USAGE ON SEQUENCE acl_chain_ids TO acl_chain_leaf WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE acl_chain_leaf",
        "GRANT USAGE ON SEQUENCE acl_chain_ids TO acl_chain_tail",
        "RESET ROLE",
        "SET ROLE acl_chain_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        sqlstate(
            &engine,
            "REVOKE GRANT OPTION FOR USAGE ON SEQUENCE acl_chain_ids FROM acl_chain_delegate RESTRICT",
        ),
        "2BP01"
    );
    for sql in [
        "GRANT USAGE ON SEQUENCE acl_chain_ids TO acl_chain_leaf WITH GRANT OPTION",
        "REVOKE GRANT OPTION FOR USAGE ON SEQUENCE acl_chain_ids FROM acl_chain_delegate CASCADE",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for (role, privilege, expected) in [
        ("acl_chain_delegate", "USAGE", true),
        ("acl_chain_delegate", "USAGE WITH GRANT OPTION", false),
        ("acl_chain_leaf", "USAGE WITH GRANT OPTION", true),
        ("acl_chain_tail", "USAGE", true),
    ] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT has_sequence_privilege('{role}', 'acl_chain_ids', '{privilege}') AS v"
                ),
            ),
            Value::Bool(expected),
            "{role} {privilege}"
        );
    }
    engine.sql("SET ROLE acl_chain_leaf", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE, UPDATE ON SEQUENCE acl_chain_ids TO acl_chain_outsider",
            &[],
        )
        .unwrap();
    assert_single_warning(
        &engine,
        "not all privileges were granted for \"acl_chain_ids\"",
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("SET ROLE acl_chain_delegate", &[]).unwrap();
    engine
        .sql(
            "GRANT UPDATE ON SEQUENCE acl_chain_ids TO acl_chain_outsider",
            &[],
        )
        .unwrap();
    assert_single_warning(&engine, "no privileges were granted for \"acl_chain_ids\"");
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(sqlstate(&engine, "DROP ROLE acl_chain_leaf"), "2BP01");
    for sql in [
        "SET ROLE acl_chain_owner",
        "REVOKE USAGE ON SEQUENCE acl_chain_ids FROM acl_chain_leaf CASCADE",
        "RESET ROLE",
        "DROP ROLE acl_chain_leaf",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('acl_chain_tail', 'acl_chain_ids', 'USAGE') AS v",
        ),
        Value::Bool(false)
    );
}

#[test]
fn sequence_acl_follows_transactions_external_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-acl.db");
    {
        let writer = Engine::open(&database).unwrap();
        for sql in [
            "CREATE ROLE acl_durable_owner",
            "CREATE ROLE acl_durable_user",
            "SET ROLE acl_durable_owner",
            "CREATE SEQUENCE acl_durable_ids",
            "RESET ROLE",
        ] {
            writer
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        let observer = Engine::open(&database).unwrap();
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(false)
        );
        writer.sql("SET ROLE acl_durable_owner", &[]).unwrap();
        writer.sql("BEGIN", &[]).unwrap();
        writer
            .sql(
                "GRANT USAGE ON SEQUENCE acl_durable_ids TO acl_durable_user",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(false)
        );
        writer.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(false)
        );
        for sql in [
            "BEGIN",
            "GRANT USAGE ON SEQUENCE acl_durable_ids TO acl_durable_user",
            "SAVEPOINT acl_before_revoke",
            "REVOKE USAGE ON SEQUENCE acl_durable_ids FROM acl_durable_user",
        ] {
            writer
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(false)
        );
        for sql in ["ROLLBACK TO SAVEPOINT acl_before_revoke", "COMMIT"] {
            writer
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_sequence_privilege('acl_durable_user', 'acl_durable_ids', 'USAGE') AS v",
            ),
            Value::Bool(true)
        );
    }
    let reopened = Engine::open(&database).unwrap();
    reopened.sql("SET ROLE acl_durable_user", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT nextval('acl_durable_ids') AS v"),
        Value::Int(1)
    );
}
