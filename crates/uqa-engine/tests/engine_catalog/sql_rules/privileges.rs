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

fn setup_rule_privileges(engine: &Engine) {
    for sql in [
        "CREATE ROLE rule_schema_owner",
        "CREATE ROLE rule_table_owner",
        "CREATE ROLE rule_table_member INHERIT",
        "CREATE ROLE rule_caller",
        "CREATE ROLE rule_outsider",
        "CREATE ROLE rule_next_owner",
        "CREATE ROLE rule_resource_owner",
        "GRANT rule_table_owner TO rule_table_member",
        "GRANT CREATE ON DATABASE uqa TO rule_schema_owner",
        "SET ROLE rule_schema_owner",
        "CREATE SCHEMA rule_privilege",
        "GRANT USAGE, CREATE ON SCHEMA rule_privilege TO rule_table_owner, rule_resource_owner",
        "GRANT USAGE ON SCHEMA rule_privilege TO rule_table_member, rule_caller, rule_outsider, rule_next_owner",
        "RESET ROLE",
        "SET ROLE rule_table_owner",
        "CREATE TABLE rule_privilege.items(id integer)",
        "CREATE TABLE rule_privilege.missing_items(id integer)",
        "CREATE TABLE rule_privilege.alter_items(id integer)",
        "CREATE TABLE rule_privilege.member_items(id integer)",
        "CREATE TABLE rule_privilege.transfer_items(id integer)",
        "CREATE TABLE rule_privilege.rollback_items(id integer)",
        "CREATE TABLE rule_privilege.runtime_items(id integer)",
        "CREATE TABLE rule_privilege.view_action_items(id integer)",
        "CREATE TABLE rule_privilege.rule_owner_source(payload integer)",
        "INSERT INTO rule_privilege.rule_owner_source VALUES (50)",
        "CREATE TABLE rule_privilege.view_base(id integer)",
        "CREATE VIEW rule_privilege.item_view AS SELECT id FROM rule_privilege.view_base",
        "CREATE VIEW rule_privilege.transfer_view AS SELECT id FROM rule_privilege.view_base",
        "CREATE RULE existing_rule AS ON INSERT TO rule_privilege.items DO NOTHING",
        "CREATE RULE alter_rule AS ON INSERT TO rule_privilege.alter_items DO NOTHING",
        "CREATE RULE member_rule AS ON INSERT TO rule_privilege.member_items DO NOTHING",
        "CREATE RULE transfer_rule AS ON INSERT TO rule_privilege.transfer_items DO NOTHING",
        "CREATE RULE rollback_rule AS ON INSERT TO rule_privilege.rollback_items DO NOTHING",
        "CREATE RULE view_insert_rule AS ON INSERT TO rule_privilege.item_view DO INSTEAD INSERT INTO rule_privilege.view_base VALUES (NEW.id)",
        "CREATE RULE transfer_view_rule AS ON INSERT TO rule_privilege.transfer_view DO INSTEAD INSERT INTO rule_privilege.view_base VALUES (NEW.id)",
        "GRANT ALL ON TABLE rule_privilege.items, rule_privilege.missing_items, rule_privilege.alter_items, rule_privilege.runtime_items, rule_privilege.view_action_items, rule_privilege.item_view TO rule_caller",
        "RESET ROLE",
        "SET ROLE rule_resource_owner",
        "CREATE TABLE rule_privilege.runtime_log(actor text, seen integer)",
        "CREATE TABLE rule_privilege.runtime_secret(payload integer)",
        "CREATE TABLE rule_privilege.view_action_base(value integer)",
        "CREATE VIEW rule_privilege.view_action AS SELECT value FROM rule_privilege.view_action_base",
        "INSERT INTO rule_privilege.runtime_secret VALUES (41)",
        "RESET ROLE",
        "GRANT INSERT ON rule_privilege.view_action TO rule_table_owner",
        "GRANT INSERT ON rule_privilege.runtime_log TO rule_table_owner",
        "GRANT SELECT ON rule_privilege.runtime_secret TO rule_table_owner",
        "SET ROLE rule_table_owner",
        "CREATE RULE runtime_rule AS ON INSERT TO rule_privilege.runtime_items DO ALSO INSERT INTO rule_privilege.runtime_log(actor, seen) SELECT current_user, payload + NEW.id FROM rule_privilege.runtime_secret",
        "CREATE RULE view_action_rule AS ON INSERT TO rule_privilege.view_action_items DO ALSO INSERT INTO rule_privilege.view_action SELECT payload + NEW.id FROM rule_privilege.rule_owner_source",
        "RESET ROLE",
    ] {
        exec(engine, sql);
    }
}

#[test]
fn rule_ddl_authority_is_derived_from_the_target_relation() {
    let engine = Engine::new();
    setup_rule_privileges(&engine);

    exec(&engine, "SET ROLE rule_caller");
    for sql in [
        "CREATE RULE denied_rule AS ON INSERT TO rule_privilege.items DO NOTHING",
        "CREATE OR REPLACE RULE existing_rule AS ON INSERT TO rule_privilege.items DO NOTHING",
    ] {
        assert_failure(&engine, sql, "42501", "must be owner of table items");
    }
    assert_failure(
        &engine,
        "DROP RULE existing_rule ON rule_privilege.items",
        "42501",
        "must be owner of relation items",
    );
    assert_failure(
        &engine,
        "ALTER RULE alter_rule ON rule_privilege.alter_items RENAME TO renamed_rule",
        "42501",
        "must be owner of table alter_items",
    );
    assert_failure(
        &engine,
        "ALTER RULE missing_rule ON rule_privilege.alter_items RENAME TO renamed_rule",
        "42501",
        "must be owner of table alter_items",
    );
    assert_failure(
        &engine,
        "ALTER TABLE rule_privilege.alter_items DISABLE RULE alter_rule",
        "42501",
        "must be owner of table alter_items",
    );
    assert_failure(
        &engine,
        "CREATE RULE denied_view_rule AS ON INSERT TO rule_privilege.item_view DO INSTEAD NOTHING",
        "42501",
        "must be owner of view item_view",
    );
    assert_failure(
        &engine,
        "ALTER RULE view_insert_rule ON rule_privilege.item_view RENAME TO renamed_view_rule",
        "42501",
        "must be owner of view item_view",
    );
    assert_eq!(
        failure(
            &engine,
            "DROP RULE missing_rule ON rule_privilege.missing_items",
        ),
        (
            "42704".into(),
            "rule \"missing_rule\" for relation \"rule_privilege.missing_items\" does not exist"
                .into(),
        )
    );
    exec(
        &engine,
        "DROP RULE IF EXISTS missing_rule ON rule_privilege.missing_items",
    );
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "NOTICE".into(),
            "rule \"missing_rule\" for relation \"rule_privilege.missing_items\" does not exist, skipping".into(),
        )]
    );
    exec(&engine, "RESET ROLE");

    exec(&engine, "SET ROLE rule_table_member");
    exec(
        &engine,
        "CREATE RULE member_created AS ON INSERT TO rule_privilege.member_items DO NOTHING",
    );
    exec(
        &engine,
        "ALTER RULE member_rule ON rule_privilege.member_items RENAME TO member_renamed",
    );
    exec(&engine, "RESET ROLE");
}

#[test]
fn rule_action_relation_privileges_use_the_relation_owner_without_changing_current_user() {
    let engine = Engine::new();
    setup_rule_privileges(&engine);

    exec(&engine, "SET ROLE rule_caller");
    exec(
        &engine,
        "INSERT INTO rule_privilege.runtime_items VALUES (1)",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        strings(
            &engine,
            "SELECT actor || ':' || seen::text AS value FROM rule_privilege.runtime_log",
            "value",
        ),
        ["rule_caller:42"]
    );
    exec(&engine, "SET ROLE rule_caller");
    exec(
        &engine,
        "INSERT INTO rule_privilege.view_action_items VALUES (1)",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        scalar(&engine, "SELECT value FROM rule_privilege.view_action_base",),
        Value::Int(51)
    );

    exec(
        &engine,
        "REVOKE INSERT ON rule_privilege.runtime_log FROM rule_table_owner",
    );
    exec(
        &engine,
        "REVOKE SELECT ON rule_privilege.runtime_secret FROM rule_table_owner",
    );
    exec(
        &engine,
        "GRANT INSERT ON rule_privilege.runtime_log TO rule_caller",
    );
    exec(
        &engine,
        "GRANT SELECT ON rule_privilege.runtime_secret TO rule_caller",
    );
    exec(&engine, "SET ROLE rule_caller");
    assert_failure(
        &engine,
        "INSERT INTO rule_privilege.runtime_items VALUES (2)",
        "42501",
        "permission denied for table runtime_log",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM rule_privilege.runtime_items",),
        Value::Int(1)
    );

    exec(
        &engine,
        "GRANT INSERT ON rule_privilege.runtime_log TO rule_table_owner",
    );
    exec(&engine, "SET ROLE rule_caller");
    assert_failure(
        &engine,
        "INSERT INTO rule_privilege.runtime_items VALUES (3)",
        "42501",
        "permission denied for table runtime_secret",
    );
    exec(&engine, "RESET ROLE");
    exec(
        &engine,
        "GRANT SELECT ON rule_privilege.runtime_secret TO rule_table_owner",
    );
    exec(&engine, "SET ROLE rule_caller");
    exec(
        &engine,
        "INSERT INTO rule_privilege.runtime_items VALUES (4)",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        strings(
            &engine,
            "SELECT actor || ':' || seen::text AS value FROM rule_privilege.runtime_log ORDER BY seen",
            "value",
        ),
        ["rule_caller:42", "rule_caller:45"]
    );
}

#[test]
fn rule_authority_tracks_owner_transfer_rollback_cross_engine_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rule-privileges.db");
    {
        let first = Engine::open(&database).unwrap();
        setup_rule_privileges(&first);

        exec(&first, "BEGIN");
        exec(
            &first,
            "ALTER TABLE rule_privilege.rollback_items OWNER TO rule_next_owner",
        );
        exec(&first, "ROLLBACK");
        exec(&first, "SET ROLE rule_table_owner");
        exec(
            &first,
            "ALTER RULE rollback_rule ON rule_privilege.rollback_items RENAME TO rollback_renamed",
        );
        exec(&first, "RESET ROLE");

        exec(
            &first,
            "ALTER TABLE rule_privilege.transfer_items OWNER TO rule_next_owner",
        );
        exec(
            &first,
            "ALTER VIEW rule_privilege.transfer_view OWNER TO rule_next_owner",
        );
        exec(
            &first,
            "ALTER TABLE rule_privilege.runtime_items OWNER TO rule_next_owner",
        );
        let second = Engine::open(&database).unwrap();
        exec(&second, "SET ROLE rule_table_owner");
        for sql in [
            "ALTER RULE transfer_rule ON rule_privilege.transfer_items RENAME TO former_rule",
            "ALTER RULE transfer_view_rule ON rule_privilege.transfer_view RENAME TO former_view_rule",
        ] {
            assert_eq!(failure(&second, sql).0, "42501", "{sql}");
        }
        exec(&second, "RESET ROLE");
        exec(&second, "SET ROLE rule_caller");
        assert_failure(
            &second,
            "INSERT INTO rule_privilege.runtime_items VALUES (5)",
            "42501",
            "permission denied for table runtime_log",
        );
        exec(&second, "RESET ROLE");
        exec(
            &second,
            "GRANT INSERT ON rule_privilege.runtime_log TO rule_next_owner",
        );
        exec(
            &second,
            "GRANT SELECT ON rule_privilege.runtime_secret TO rule_next_owner",
        );
    }

    let reopened = Engine::open(&database).unwrap();
    exec(&reopened, "SET ROLE rule_next_owner");
    exec(
        &reopened,
        "ALTER RULE transfer_rule ON rule_privilege.transfer_items RENAME TO next_rule",
    );
    exec(
        &reopened,
        "ALTER RULE transfer_view_rule ON rule_privilege.transfer_view RENAME TO next_view_rule",
    );
    exec(&reopened, "RESET ROLE");
    exec(&reopened, "SET ROLE rule_caller");
    exec(
        &reopened,
        "INSERT INTO rule_privilege.runtime_items VALUES (6)",
    );
    exec(&reopened, "RESET ROLE");
    assert_eq!(
        strings(
            &reopened,
            "SELECT actor || ':' || seen::text AS value FROM rule_privilege.runtime_log",
            "value",
        ),
        ["rule_caller:47"]
    );
}
