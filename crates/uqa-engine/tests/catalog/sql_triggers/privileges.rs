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

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = exec(engine, sql);
    result.rows[0][&result.columns[0]].clone()
}

fn setup_trigger_privileges(engine: &Engine) {
    for sql in [
        "CREATE ROLE trigger_schema_owner",
        "CREATE ROLE trigger_table_owner",
        "CREATE ROLE trigger_table_member INHERIT",
        "CREATE ROLE trigger_function_owner",
        "CREATE ROLE trigger_creator",
        "CREATE ROLE trigger_outsider",
        "CREATE ROLE trigger_next_owner",
        "GRANT trigger_table_owner TO trigger_table_member",
        "GRANT CREATE ON DATABASE uqa TO trigger_schema_owner",
        "SET ROLE trigger_schema_owner",
        "CREATE SCHEMA trigger_privilege",
        "GRANT USAGE, CREATE ON SCHEMA trigger_privilege TO trigger_table_owner, trigger_function_owner",
        "GRANT USAGE ON SCHEMA trigger_privilege TO trigger_table_member, trigger_creator, trigger_outsider, trigger_next_owner",
        "RESET ROLE",
        "SET ROLE trigger_function_owner",
        "CREATE FUNCTION trigger_privilege.allowed_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE FUNCTION trigger_privilege.denied_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE FUNCTION trigger_privilege.not_a_trigger() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "REVOKE ALL ON FUNCTION trigger_privilege.allowed_trigger() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION trigger_privilege.denied_trigger() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION trigger_privilege.not_a_trigger() FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION trigger_privilege.allowed_trigger() TO trigger_table_owner, trigger_creator, trigger_schema_owner, trigger_outsider",
        "RESET ROLE",
        "SET ROLE trigger_table_owner",
        "CREATE TABLE trigger_privilege.items(id integer)",
        "CREATE TABLE trigger_privilege.denied_items(id integer)",
        "CREATE TABLE trigger_privilege.execute_items(id integer)",
        "CREATE TABLE trigger_privilege.member_items(id integer)",
        "CREATE TABLE trigger_privilege.creator_drop_items(id integer)",
        "CREATE TABLE trigger_privilege.schema_drop_items(id integer)",
        "CREATE TABLE trigger_privilege.member_drop_items(id integer)",
        "CREATE TABLE trigger_privilege.owner_drop_items(id integer)",
        "CREATE TABLE trigger_privilege.missing_drop_items(id integer)",
        "CREATE TABLE trigger_privilege.alter_creator_items(id integer)",
        "CREATE TABLE trigger_privilege.alter_member_items(id integer)",
        "CREATE TABLE trigger_privilege.transfer_items(id integer)",
        "CREATE TABLE trigger_privilege.rollback_items(id integer)",
        "CREATE TABLE trigger_privilege.runtime_items(id integer)",
        "CREATE TABLE trigger_privilege.view_base(id integer)",
        "CREATE VIEW trigger_privilege.item_view AS SELECT id FROM trigger_privilege.view_base",
        "CREATE VIEW trigger_privilege.transfer_view AS SELECT id FROM trigger_privilege.view_base",
        "GRANT TRIGGER ON TABLE trigger_privilege.items, trigger_privilege.execute_items, trigger_privilege.creator_drop_items, trigger_privilege.alter_creator_items, trigger_privilege.runtime_items, trigger_privilege.item_view TO trigger_creator",
        "GRANT INSERT, SELECT ON TABLE trigger_privilege.runtime_items TO trigger_creator",
        "CREATE TRIGGER creator_drop_trigger BEFORE INSERT ON trigger_privilege.creator_drop_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER schema_drop_trigger BEFORE INSERT ON trigger_privilege.schema_drop_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER member_drop_trigger BEFORE INSERT ON trigger_privilege.member_drop_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER owner_drop_trigger BEFORE INSERT ON trigger_privilege.owner_drop_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER alter_creator_trigger BEFORE INSERT ON trigger_privilege.alter_creator_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER alter_member_trigger BEFORE INSERT ON trigger_privilege.alter_member_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER transfer_trigger BEFORE INSERT ON trigger_privilege.transfer_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER rollback_trigger BEFORE INSERT ON trigger_privilege.rollback_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER view_drop_trigger INSTEAD OF INSERT ON trigger_privilege.item_view FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER transfer_view_trigger INSTEAD OF INSERT ON trigger_privilege.transfer_view FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "RESET ROLE",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn trigger_creation_checks_target_then_function_privileges_in_pg18_order() {
    let engine = Engine::new();
    setup_trigger_privileges(&engine);

    exec(&engine, "SET ROLE trigger_creator");
    for sql in [
        "CREATE TRIGGER denied_table_trigger BEFORE INSERT ON trigger_privilege.denied_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER missing_function_trigger BEFORE INSERT ON trigger_privilege.denied_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.missing_trigger()",
        "CREATE TRIGGER wrong_return_trigger BEFORE INSERT ON trigger_privilege.denied_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.not_a_trigger()",
    ] {
        assert_failure(
            &engine,
            sql,
            "42501",
            "permission denied for table denied_items",
        );
    }
    assert_failure(
        &engine,
        "CREATE TRIGGER denied_function_trigger BEFORE INSERT ON trigger_privilege.execute_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.denied_trigger()",
        "42501",
        "permission denied for function trigger_privilege.denied_trigger",
    );
    assert_failure(
        &engine,
        "CREATE TRIGGER wrong_return_trigger BEFORE INSERT ON trigger_privilege.execute_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.not_a_trigger()",
        "42501",
        "permission denied for function trigger_privilege.not_a_trigger",
    );
    exec(
        &engine,
        "CREATE TRIGGER granted_trigger BEFORE INSERT ON trigger_privilege.items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    );
    exec(
        &engine,
        "CREATE CONSTRAINT TRIGGER granted_constraint_trigger AFTER INSERT ON trigger_privilege.items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER view_granted_trigger BEFORE INSERT ON trigger_privilege.item_view FOR EACH STATEMENT EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    );
    exec(
        &engine,
        "CREATE OR REPLACE TRIGGER granted_trigger AFTER INSERT ON trigger_privilege.items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_outsider");
    assert_failure(
        &engine,
        "CREATE TRIGGER view_denied_trigger BEFORE INSERT ON trigger_privilege.item_view FOR EACH STATEMENT EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "42501",
        "permission denied for view item_view",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_schema_owner");
    assert_failure(
        &engine,
        "CREATE TRIGGER schema_owner_trigger BEFORE INSERT ON trigger_privilege.denied_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "42501",
        "permission denied for table denied_items",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_table_member");
    exec(
        &engine,
        "CREATE TRIGGER member_create_trigger BEFORE INSERT ON trigger_privilege.member_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    );
    exec(&engine, "RESET ROLE");

    exec(
        &engine,
        "REVOKE EXECUTE ON FUNCTION trigger_privilege.allowed_trigger() FROM trigger_creator",
    );
    exec(&engine, "SET ROLE trigger_creator");
    for sql in [
        "CREATE OR REPLACE TRIGGER granted_trigger BEFORE INSERT ON trigger_privilege.items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        "CREATE TRIGGER granted_trigger BEFORE INSERT ON trigger_privilege.items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
    ] {
        assert_failure(
            &engine,
            sql,
            "42501",
            "permission denied for function trigger_privilege.allowed_trigger",
        );
    }
    exec(&engine, "RESET ROLE");
}

#[test]
fn trigger_drop_and_alter_derive_authority_from_the_target_relation() {
    let engine = Engine::new();
    setup_trigger_privileges(&engine);

    exec(&engine, "SET ROLE trigger_creator");
    assert_eq!(
        failure(
            &engine,
            "DROP TRIGGER creator_drop_trigger ON trigger_privilege.creator_drop_items",
        ),
        (
            "42501".into(),
            "must be owner of relation creator_drop_items".into()
        )
    );
    assert_eq!(
        failure(
            &engine,
            "ALTER TRIGGER alter_creator_trigger ON trigger_privilege.alter_creator_items RENAME TO creator_renamed_trigger",
        ),
        (
            "42501".into(),
            "must be owner of table alter_creator_items".into()
        )
    );
    assert_eq!(
        failure(
            &engine,
            "DROP TRIGGER view_drop_trigger ON trigger_privilege.item_view",
        ),
        ("42501".into(), "must be owner of relation item_view".into())
    );
    assert_eq!(
        failure(
            &engine,
            "ALTER TRIGGER view_drop_trigger ON trigger_privilege.item_view RENAME TO view_creator_renamed_trigger",
        ),
        (
            "42501".into(),
            "must be owner of view item_view".into()
        )
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_schema_owner");
    assert_eq!(
        failure(
            &engine,
            "DROP TRIGGER schema_drop_trigger ON trigger_privilege.schema_drop_items",
        ),
        (
            "42501".into(),
            "must be owner of relation schema_drop_items".into()
        )
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_outsider");
    assert_eq!(
        failure(
            &engine,
            "DROP TRIGGER missing_trigger ON trigger_privilege.missing_drop_items",
        ),
        (
            "42704".into(),
            "trigger \"missing_trigger\" for table \"missing_drop_items\" does not exist".into()
        )
    );
    exec(
        &engine,
        "DROP TRIGGER IF EXISTS missing_trigger ON trigger_privilege.missing_drop_items",
    );
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "NOTICE".into(),
            "trigger \"missing_trigger\" for relation \"trigger_privilege.missing_drop_items\" does not exist, skipping".into()
        )]
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_table_member");
    exec(
        &engine,
        "DROP TRIGGER member_drop_trigger ON trigger_privilege.member_drop_items",
    );
    exec(
        &engine,
        "ALTER TRIGGER alter_member_trigger ON trigger_privilege.alter_member_items RENAME TO member_renamed_trigger",
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE trigger_table_owner");
    exec(
        &engine,
        "DROP TRIGGER owner_drop_trigger ON trigger_privilege.owner_drop_items",
    );
    exec(
        &engine,
        "DROP TRIGGER view_drop_trigger ON trigger_privilege.item_view",
    );
    exec(&engine, "RESET ROLE");
}

#[test]
fn trigger_authority_tracks_acl_revocation_owner_transfer_rollback_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("trigger-privileges.db");
    {
        let first = Engine::open(&database).unwrap();
        setup_trigger_privileges(&first);

        exec(&first, "SET ROLE trigger_creator");
        exec(
            &first,
            "CREATE TRIGGER runtime_trigger BEFORE INSERT ON trigger_privilege.runtime_items FOR EACH ROW EXECUTE FUNCTION trigger_privilege.allowed_trigger()",
        );
        exec(&first, "RESET ROLE");
        exec(
            &first,
            "REVOKE EXECUTE ON FUNCTION trigger_privilege.allowed_trigger() FROM trigger_creator",
        );
        exec(&first, "SET ROLE trigger_creator");
        exec(
            &first,
            "INSERT INTO trigger_privilege.runtime_items VALUES (1)",
        );
        assert_eq!(
            scalar(&first, "SELECT id FROM trigger_privilege.runtime_items"),
            Value::Int(1)
        );
        exec(&first, "RESET ROLE");

        exec(&first, "BEGIN");
        exec(
            &first,
            "ALTER TABLE trigger_privilege.rollback_items OWNER TO trigger_next_owner",
        );
        exec(&first, "ROLLBACK");
        exec(&first, "SET ROLE trigger_table_owner");
        exec(
            &first,
            "DROP TRIGGER rollback_trigger ON trigger_privilege.rollback_items",
        );
        exec(&first, "RESET ROLE");

        exec(
            &first,
            "ALTER TABLE trigger_privilege.transfer_items OWNER TO trigger_next_owner",
        );
        exec(
            &first,
            "ALTER VIEW trigger_privilege.transfer_view OWNER TO trigger_next_owner",
        );
        let second = Engine::open(&database).unwrap();
        exec(&second, "SET ROLE trigger_table_owner");
        for sql in [
            "DROP TRIGGER transfer_trigger ON trigger_privilege.transfer_items",
            "DROP TRIGGER transfer_view_trigger ON trigger_privilege.transfer_view",
        ] {
            assert_eq!(failure(&second, sql).0, "42501", "{sql}");
        }
        exec(&second, "RESET ROLE");
    }

    let reopened = Engine::open(&database).unwrap();
    exec(&reopened, "SET ROLE trigger_next_owner");
    exec(
        &reopened,
        "DROP TRIGGER transfer_trigger ON trigger_privilege.transfer_items",
    );
    exec(
        &reopened,
        "DROP TRIGGER transfer_view_trigger ON trigger_privilege.transfer_view",
    );
}
