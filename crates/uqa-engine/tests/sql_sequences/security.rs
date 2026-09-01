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

fn scalar(engine: &Engine, sql: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0]["v"].clone()
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
