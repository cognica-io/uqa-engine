//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

fn assert_failure(engine: &Engine, sql: &str, state: &str, message: &str) {
    assert_eq!(
        failure(engine, sql),
        (state.to_string(), message.to_string()),
        "{sql}"
    );
}

fn setup_foreign_trigger_table(engine: &Engine) {
    for sql in [
        "CREATE SERVER foreign_trigger_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE foreign_trigger_items(id integer, value text) SERVER foreign_trigger_memory OPTIONS (source 'memory')",
        "CREATE FUNCTION foreign_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
    ] {
        exec(engine, sql);
    }
}

fn assert_foreign_trigger_catalog(engine: &Engine) {
    let class = exec(
        engine,
        "SELECT relkind, relhastriggers FROM pg_class WHERE oid = 'foreign_trigger_items'::regclass",
    );
    assert_eq!(class.rows[0].get("relkind"), Some(&Value::Str("f".into())));
    assert_eq!(
        class.rows[0].get("relhastriggers"),
        Some(&Value::Bool(true))
    );

    let rows = exec(
        engine,
        "SELECT tgname, tgtype, tgenabled, pg_get_triggerdef(oid, false) AS definition FROM pg_trigger WHERE tgrelid = 'foreign_trigger_items'::regclass ORDER BY tgname",
    )
    .rows;
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[0].get("tgname"),
        Some(&Value::Str("delete_before".into()))
    );
    assert_eq!(rows[0].get("tgtype"), Some(&Value::Int(11)));
    assert_eq!(
        rows[1].get("tgname"),
        Some(&Value::Str("row_before".into()))
    );
    assert_eq!(rows[1].get("tgtype"), Some(&Value::Int(23)));
    assert_eq!(
        rows[2].get("tgname"),
        Some(&Value::Str("statement_after".into()))
    );
    assert_eq!(rows[2].get("tgtype"), Some(&Value::Int(28)));
    assert_eq!(
        rows[3].get("tgname"),
        Some(&Value::Str("truncate_after".into()))
    );
    assert_eq!(rows[3].get("tgtype"), Some(&Value::Int(32)));
    assert_eq!(
        rows[1].get("definition"),
        Some(&Value::Str(
            "CREATE TRIGGER row_before BEFORE INSERT OR UPDATE OF value ON public.foreign_trigger_items FOR EACH ROW WHEN ((new.id > 0)) EXECUTE FUNCTION foreign_trigger_probe()".into()
        ))
    );
}

#[test]
fn foreign_table_trigger_definitions_and_catalogs_match_postgresql_18() {
    let engine = Engine::new();
    setup_foreign_trigger_table(&engine);

    for sql in [
        "CREATE CONSTRAINT TRIGGER invalid_constraint AFTER INSERT ON foreign_trigger_items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_probe()",
        "CREATE TRIGGER invalid_transition AFTER INSERT ON foreign_trigger_items REFERENCING NEW TABLE AS inserted FOR EACH STATEMENT EXECUTE FUNCTION foreign_trigger_probe()",
        "CREATE TRIGGER invalid_instead INSTEAD OF INSERT ON foreign_trigger_items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_probe()",
    ] {
        assert_failure(
            &engine,
            sql,
            "42809",
            "\"foreign_trigger_items\" is a foreign table",
        );
    }

    for sql in [
        "CREATE TRIGGER row_before BEFORE INSERT OR UPDATE OF value ON foreign_trigger_items FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION foreign_trigger_probe()",
        "CREATE TRIGGER delete_before BEFORE DELETE ON foreign_trigger_items FOR EACH ROW WHEN (OLD.id > 0) EXECUTE FUNCTION foreign_trigger_probe()",
        "CREATE TRIGGER statement_after AFTER INSERT OR UPDATE OR DELETE ON foreign_trigger_items FOR EACH STATEMENT EXECUTE FUNCTION foreign_trigger_probe()",
        "CREATE TRIGGER truncate_after AFTER TRUNCATE ON foreign_trigger_items FOR EACH STATEMENT EXECUTE FUNCTION foreign_trigger_probe()",
    ] {
        exec(&engine, sql);
    }
    exec(
        &engine,
        "CREATE OR REPLACE TRIGGER statement_after AFTER INSERT OR UPDATE OR DELETE ON foreign_trigger_items FOR EACH STATEMENT EXECUTE FUNCTION foreign_trigger_probe()",
    );

    assert_foreign_trigger_catalog(&engine);

    exec(
        &engine,
        "ALTER FOREIGN TABLE foreign_trigger_items DISABLE TRIGGER row_before",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT tgenabled FROM pg_trigger WHERE tgname = 'row_before'",
        )
        .rows[0]
            .get("tgenabled"),
        Some(&Value::Str("D".into()))
    );
    exec(
        &engine,
        "ALTER TABLE foreign_trigger_items ENABLE ALWAYS TRIGGER row_before",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT tgenabled FROM pg_trigger WHERE tgname = 'row_before'",
        )
        .rows[0]
            .get("tgenabled"),
        Some(&Value::Str("A".into()))
    );
    exec(
        &engine,
        "ALTER TRIGGER row_before ON foreign_trigger_items RENAME TO renamed_before",
    );
    exec(
        &engine,
        "DROP TRIGGER renamed_before ON foreign_trigger_items",
    );
}

fn setup_foreign_trigger_privileges(engine: &Engine) {
    for sql in [
        "CREATE ROLE foreign_trigger_schema_owner",
        "CREATE ROLE foreign_trigger_owner",
        "CREATE ROLE foreign_trigger_function_owner",
        "CREATE ROLE foreign_trigger_creator",
        "CREATE ROLE foreign_trigger_outsider",
        "CREATE ROLE foreign_trigger_next_owner",
        "GRANT CREATE ON DATABASE uqa TO foreign_trigger_schema_owner",
        "CREATE SERVER foreign_trigger_acl_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "SET ROLE foreign_trigger_schema_owner",
        "CREATE SCHEMA foreign_trigger_acl",
        "GRANT USAGE, CREATE ON SCHEMA foreign_trigger_acl TO foreign_trigger_owner, foreign_trigger_function_owner",
        "GRANT USAGE ON SCHEMA foreign_trigger_acl TO foreign_trigger_creator, foreign_trigger_outsider, foreign_trigger_next_owner",
        "RESET ROLE",
        "SET ROLE foreign_trigger_function_owner",
        "CREATE FUNCTION foreign_trigger_acl.allowed_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE FUNCTION foreign_trigger_acl.denied_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "REVOKE ALL ON FUNCTION foreign_trigger_acl.allowed_trigger() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION foreign_trigger_acl.denied_trigger() FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION foreign_trigger_acl.allowed_trigger() TO foreign_trigger_owner, foreign_trigger_creator, foreign_trigger_outsider",
        "RESET ROLE",
        "SET ROLE foreign_trigger_owner",
        "CREATE FOREIGN TABLE foreign_trigger_acl.items(id integer) SERVER foreign_trigger_acl_memory OPTIONS (source 'memory')",
        "CREATE FOREIGN TABLE foreign_trigger_acl.transfer_items(id integer) SERVER foreign_trigger_acl_memory OPTIONS (source 'memory')",
        "CREATE FOREIGN TABLE foreign_trigger_acl.rollback_items(id integer) SERVER foreign_trigger_acl_memory OPTIONS (source 'memory')",
        "CREATE TRIGGER owner_trigger BEFORE INSERT ON foreign_trigger_acl.items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
        "CREATE TRIGGER transfer_trigger BEFORE INSERT ON foreign_trigger_acl.transfer_items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
        "CREATE TRIGGER rollback_trigger BEFORE INSERT ON foreign_trigger_acl.rollback_items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
        "RESET ROLE",
        "GRANT TRIGGER ON TABLE foreign_trigger_acl.items TO foreign_trigger_creator",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn foreign_table_trigger_privileges_derive_from_foreign_table_state() {
    let engine = Engine::new();
    setup_foreign_trigger_privileges(&engine);

    exec(&engine, "SET ROLE foreign_trigger_creator");
    exec(
        &engine,
        "CREATE TRIGGER creator_trigger BEFORE INSERT ON foreign_trigger_acl.items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
    );
    assert_failure(
        &engine,
        "CREATE TRIGGER denied_function BEFORE INSERT ON foreign_trigger_acl.items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.denied_trigger()",
        "42501",
        "permission denied for function foreign_trigger_acl.denied_trigger",
    );
    assert_failure(
        &engine,
        "ALTER FOREIGN TABLE foreign_trigger_acl.items DISABLE TRIGGER creator_trigger",
        "42501",
        "must be owner of foreign table items",
    );
    assert_failure(
        &engine,
        "ALTER TRIGGER creator_trigger ON foreign_trigger_acl.items RENAME TO denied_rename",
        "42501",
        "must be owner of foreign table items",
    );
    assert_failure(
        &engine,
        "DROP TRIGGER creator_trigger ON foreign_trigger_acl.items",
        "42501",
        "must be owner of relation items",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE foreign_trigger_outsider");
    assert_failure(
        &engine,
        "CREATE TRIGGER outsider_trigger BEFORE INSERT ON foreign_trigger_acl.items FOR EACH ROW EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
        "42501",
        "permission denied for foreign table items",
    );
    assert_failure(
        &engine,
        "CREATE TRIGGER outsider_transition AFTER INSERT ON foreign_trigger_acl.items REFERENCING NEW TABLE AS inserted FOR EACH STATEMENT EXECUTE FUNCTION foreign_trigger_acl.allowed_trigger()",
        "42501",
        "permission denied for foreign table items",
    );
    assert_failure(
        &engine,
        "DROP TRIGGER missing_trigger ON foreign_trigger_acl.items",
        "42704",
        "trigger \"missing_trigger\" for table \"items\" does not exist",
    );
    exec(
        &engine,
        "DROP TRIGGER IF EXISTS missing_trigger ON foreign_trigger_acl.items",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE foreign_trigger_owner");
    exec(
        &engine,
        "ALTER FOREIGN TABLE foreign_trigger_acl.items DISABLE TRIGGER creator_trigger",
    );
    exec(
        &engine,
        "ALTER TRIGGER creator_trigger ON foreign_trigger_acl.items RENAME TO renamed_trigger",
    );
    exec(
        &engine,
        "DROP TRIGGER renamed_trigger ON foreign_trigger_acl.items",
    );
    exec(&engine, "RESET ROLE");
}

#[test]
fn foreign_table_trigger_lifecycle_tracks_owner_transfer_rollback_drop_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("foreign-table-triggers.db");
    {
        let first = Engine::open(&database).unwrap();
        setup_foreign_trigger_privileges(&first);

        exec(&first, "BEGIN");
        exec(
            &first,
            "ALTER FOREIGN TABLE foreign_trigger_acl.rollback_items OWNER TO foreign_trigger_next_owner",
        );
        exec(&first, "ROLLBACK");
        exec(&first, "SET ROLE foreign_trigger_owner");
        exec(
            &first,
            "DROP TRIGGER rollback_trigger ON foreign_trigger_acl.rollback_items",
        );
        exec(&first, "RESET ROLE");

        exec(
            &first,
            "ALTER FOREIGN TABLE foreign_trigger_acl.items DISABLE TRIGGER owner_trigger",
        );
        let second = Engine::open(&database).unwrap();
        assert_eq!(
            exec(
                &second,
                "SELECT tgenabled FROM pg_trigger WHERE tgname = 'owner_trigger'",
            )
            .rows[0]
                .get("tgenabled"),
            Some(&Value::Str("D".into()))
        );
        exec(
            &first,
            "ALTER FOREIGN TABLE foreign_trigger_acl.transfer_items OWNER TO foreign_trigger_next_owner",
        );
        exec(&second, "SET ROLE foreign_trigger_owner");
        assert_failure(
            &second,
            "DROP TRIGGER transfer_trigger ON foreign_trigger_acl.transfer_items",
            "42501",
            "must be owner of relation transfer_items",
        );
        exec(&second, "RESET ROLE");
    }

    let reopened = Engine::open(&database).unwrap();
    exec(&reopened, "SET ROLE foreign_trigger_next_owner");
    exec(
        &reopened,
        "DROP TRIGGER transfer_trigger ON foreign_trigger_acl.transfer_items",
    );
    exec(&reopened, "RESET ROLE");

    exec(&reopened, "BEGIN");
    exec(&reopened, "DROP FOREIGN TABLE foreign_trigger_acl.items");
    exec(&reopened, "ROLLBACK");
    assert_eq!(
        exec(
            &reopened,
            "SELECT count(*) AS count FROM pg_trigger WHERE tgname = 'owner_trigger'",
        )
        .rows[0]
            .get("count"),
        Some(&Value::Int(1))
    );
    let dependency = reopened
        .sql("DROP FUNCTION foreign_trigger_acl.allowed_trigger()", &[])
        .expect_err("foreign-table trigger must retain its function dependency");
    assert_eq!(dependency.sqlstate(), Some("2BP01"));
    assert!(
        dependency.to_string().contains("owner_trigger"),
        "{dependency}"
    );
    exec(&reopened, "DROP FOREIGN TABLE foreign_trigger_acl.items");
    exec(
        &reopened,
        "DROP FUNCTION foreign_trigger_acl.allowed_trigger()",
    );
    assert_eq!(
        exec(
            &reopened,
            "SELECT count(*) AS count FROM pg_trigger WHERE tgname = 'owner_trigger'",
        )
        .rows[0]
            .get("count"),
        Some(&Value::Int(0))
    );
}
