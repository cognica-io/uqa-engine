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

fn setup_rule_privilege_subjects(engine: &Engine) {
    for sql in [
        "CREATE ROLE rule_expression_owner",
        "CREATE ROLE rule_expression_caller",
        "CREATE ROLE rule_expression_next_owner",
        "CREATE ROLE rule_expression_resource_owner",
        "GRANT CREATE ON DATABASE uqa TO rule_expression_owner",
        "SET ROLE rule_expression_owner",
        "CREATE SCHEMA rule_expression",
        "GRANT USAGE, CREATE ON SCHEMA rule_expression TO rule_expression_resource_owner, rule_expression_next_owner",
        "GRANT USAGE ON SCHEMA rule_expression TO rule_expression_caller",
        "CREATE TABLE rule_expression.action_log(kind text, value bigint)",
        "CREATE TABLE rule_expression.routine_owner_event(id bigint)",
        "CREATE TABLE rule_expression.routine_caller_event(id bigint)",
        "CREATE TABLE rule_expression.condition_owner_event(id bigint)",
        "CREATE TABLE rule_expression.condition_caller_event(id bigint)",
        "CREATE TABLE rule_expression.nextval_owner_event(id bigint)",
        "CREATE TABLE rule_expression.nextval_caller_event(id bigint)",
        "CREATE TABLE rule_expression.currval_owner_event(id bigint)",
        "CREATE TABLE rule_expression.currval_caller_event(id bigint)",
        "CREATE TABLE rule_expression.lastval_event(id bigint)",
        "CREATE TABLE rule_expression.setval_owner_event(id bigint)",
        "CREATE TABLE rule_expression.setval_caller_event(id bigint)",
        "CREATE TABLE rule_expression.sequence_owner_scan_event(id bigint)",
        "CREATE TABLE rule_expression.sequence_caller_scan_event(id bigint)",
        "CREATE TABLE rule_expression.default_owner_event(id bigint)",
        "CREATE TABLE rule_expression.default_caller_event(id bigint)",
        "GRANT INSERT ON rule_expression.routine_owner_event, rule_expression.routine_caller_event, rule_expression.condition_owner_event, rule_expression.condition_caller_event, rule_expression.nextval_owner_event, rule_expression.nextval_caller_event, rule_expression.currval_owner_event, rule_expression.currval_caller_event, rule_expression.lastval_event, rule_expression.setval_owner_event, rule_expression.setval_caller_event, rule_expression.sequence_owner_scan_event, rule_expression.sequence_caller_scan_event, rule_expression.default_owner_event, rule_expression.default_caller_event TO rule_expression_caller",
        "RESET ROLE",
        "SET ROLE rule_expression_resource_owner",
        "CREATE FUNCTION rule_expression.owner_only() RETURNS bigint LANGUAGE SQL AS 'SELECT 101::bigint'",
        "CREATE FUNCTION rule_expression.caller_only() RETURNS bigint LANGUAGE SQL AS 'SELECT 102::bigint'",
        "REVOKE ALL ON FUNCTION rule_expression.owner_only() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION rule_expression.caller_only() FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION rule_expression.owner_only() TO rule_expression_owner",
        "GRANT EXECUTE ON FUNCTION rule_expression.caller_only() TO rule_expression_caller",
        "CREATE SEQUENCE rule_expression.owner_sequence",
        "CREATE SEQUENCE rule_expression.caller_sequence",
        "REVOKE ALL ON SEQUENCE rule_expression.owner_sequence, rule_expression.caller_sequence FROM PUBLIC",
        "GRANT USAGE, SELECT, UPDATE ON SEQUENCE rule_expression.owner_sequence TO rule_expression_owner",
        "GRANT USAGE, SELECT, UPDATE ON SEQUENCE rule_expression.caller_sequence TO rule_expression_caller",
        "CREATE TABLE rule_expression.default_owner_target(id bigint DEFAULT nextval('rule_expression.owner_sequence'))",
        "CREATE TABLE rule_expression.default_caller_target(id bigint DEFAULT nextval('rule_expression.caller_sequence'))",
        "GRANT INSERT ON rule_expression.default_owner_target, rule_expression.default_caller_target TO rule_expression_owner, rule_expression_next_owner",
        "RESET ROLE",
        "SET ROLE rule_expression_owner",
        "CREATE RULE routine_owner_rule AS ON INSERT TO rule_expression.routine_owner_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('routine-owner', rule_expression.owner_only())",
        "CREATE RULE routine_caller_rule AS ON INSERT TO rule_expression.routine_caller_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('routine-caller', rule_expression.caller_only())",
        "CREATE RULE condition_owner_rule AS ON INSERT TO rule_expression.condition_owner_event WHERE rule_expression.owner_only() = 101 DO ALSO INSERT INTO rule_expression.action_log VALUES ('condition-owner', NEW.id)",
        "CREATE RULE condition_caller_rule AS ON INSERT TO rule_expression.condition_caller_event WHERE rule_expression.caller_only() = 102 DO ALSO INSERT INTO rule_expression.action_log VALUES ('condition-caller', NEW.id)",
        "CREATE RULE nextval_owner_rule AS ON INSERT TO rule_expression.nextval_owner_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('nextval-owner', nextval('rule_expression.owner_sequence'))",
        "CREATE RULE nextval_caller_rule AS ON INSERT TO rule_expression.nextval_caller_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('nextval-caller', nextval('rule_expression.caller_sequence'))",
        "CREATE RULE currval_owner_rule AS ON INSERT TO rule_expression.currval_owner_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('currval-owner', currval('rule_expression.owner_sequence'))",
        "CREATE RULE currval_caller_rule AS ON INSERT TO rule_expression.currval_caller_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('currval-caller', currval('rule_expression.caller_sequence'))",
        "CREATE RULE lastval_rule AS ON INSERT TO rule_expression.lastval_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('lastval', lastval())",
        "CREATE RULE setval_owner_rule AS ON INSERT TO rule_expression.setval_owner_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('setval-owner', setval('rule_expression.owner_sequence', NEW.id))",
        "CREATE RULE setval_caller_rule AS ON INSERT TO rule_expression.setval_caller_event DO ALSO INSERT INTO rule_expression.action_log VALUES ('setval-caller', setval('rule_expression.caller_sequence', NEW.id))",
        "CREATE RULE sequence_owner_scan_rule AS ON INSERT TO rule_expression.sequence_owner_scan_event DO ALSO INSERT INTO rule_expression.action_log SELECT 'sequence-owner-scan', last_value FROM rule_expression.owner_sequence",
        "CREATE RULE sequence_caller_scan_rule AS ON INSERT TO rule_expression.sequence_caller_scan_event DO ALSO INSERT INTO rule_expression.action_log SELECT 'sequence-caller-scan', last_value FROM rule_expression.caller_sequence",
        "CREATE RULE default_owner_rule AS ON INSERT TO rule_expression.default_owner_event DO ALSO INSERT INTO rule_expression.default_owner_target DEFAULT VALUES",
        "CREATE RULE default_caller_rule AS ON INSERT TO rule_expression.default_caller_event DO ALSO INSERT INTO rule_expression.default_caller_target DEFAULT VALUES",
        "RESET ROLE",
    ] {
        exec(engine, sql);
    }
}

fn assert_routine_privilege_subjects(engine: &Engine) {
    assert_failure(
        engine,
        "INSERT INTO rule_expression.routine_owner_event VALUES (1)",
        "42501",
        "permission denied for function owner_only",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.routine_caller_event VALUES (1)",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.condition_owner_event VALUES (1)",
        "42501",
        "permission denied for function owner_only",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.condition_caller_event VALUES (7)",
    );
}

fn assert_sequence_function_privilege_subjects(engine: &Engine) {
    assert_failure(
        engine,
        "INSERT INTO rule_expression.nextval_owner_event VALUES (1)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.currval_owner_event VALUES (1)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.lastval_event VALUES (1)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.nextval_caller_event VALUES (1)",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.currval_caller_event VALUES (1)",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.lastval_event VALUES (1)",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.setval_owner_event VALUES (20)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.setval_caller_event VALUES (20)",
    );
}

fn assert_sequence_relation_and_default_privilege_subjects(engine: &Engine) {
    exec(
        engine,
        "INSERT INTO rule_expression.sequence_owner_scan_event VALUES (1)",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.sequence_caller_scan_event VALUES (1)",
        "42501",
        "permission denied for sequence caller_sequence",
    );
    assert_failure(
        engine,
        "INSERT INTO rule_expression.default_owner_event VALUES (1)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    exec(
        engine,
        "INSERT INTO rule_expression.default_caller_event VALUES (1)",
    );
}

fn assert_privilege_subject_results(engine: &Engine) {
    assert_eq!(
        strings(
            engine,
            "SELECT kind || ':' || value::text AS entry FROM rule_expression.action_log ORDER BY kind",
            "entry",
        ),
        [
            "condition-caller:7",
            "currval-caller:1",
            "lastval:1",
            "nextval-caller:1",
            "routine-caller:102",
            "sequence-owner-scan:1",
            "setval-caller:20",
        ]
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT id FROM rule_expression.default_caller_target",
        ),
        Value::Int(21)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) FROM rule_expression.default_owner_target",
        ),
        Value::Int(0)
    );
}

#[test]
fn rule_expression_privileges_distinguish_relation_owner_from_invoker() {
    let engine = Engine::new();
    setup_rule_privilege_subjects(&engine);

    assert_eq!(
        scalar(
            &engine,
            "SELECT nextval('rule_expression.owner_sequence') AS value",
        ),
        Value::Int(1)
    );
    exec(&engine, "SET ROLE rule_expression_caller");
    assert_routine_privilege_subjects(&engine);
    assert_sequence_function_privilege_subjects(&engine);
    assert_sequence_relation_and_default_privilege_subjects(&engine);
    exec(&engine, "RESET ROLE");
    assert_privilege_subject_results(&engine);
}

#[test]
fn rule_expression_invoker_subject_does_not_follow_relation_owner_transfer() {
    let engine = Engine::new();
    setup_rule_privilege_subjects(&engine);

    exec(
        &engine,
        "ALTER TABLE rule_expression.routine_owner_event OWNER TO rule_expression_next_owner",
    );
    exec(
        &engine,
        "ALTER TABLE rule_expression.nextval_owner_event OWNER TO rule_expression_next_owner",
    );
    exec(
        &engine,
        "GRANT INSERT ON rule_expression.action_log TO rule_expression_next_owner",
    );
    exec(
        &engine,
        "GRANT EXECUTE ON FUNCTION rule_expression.owner_only() TO rule_expression_next_owner",
    );
    exec(
        &engine,
        "GRANT USAGE ON SEQUENCE rule_expression.owner_sequence TO rule_expression_next_owner",
    );
    exec(&engine, "SET ROLE rule_expression_caller");
    assert_failure(
        &engine,
        "INSERT INTO rule_expression.routine_owner_event VALUES (1)",
        "42501",
        "permission denied for function owner_only",
    );
    assert_failure(
        &engine,
        "INSERT INTO rule_expression.nextval_owner_event VALUES (1)",
        "42501",
        "permission denied for sequence owner_sequence",
    );
    exec(&engine, "RESET ROLE");

    exec(
        &engine,
        "GRANT EXECUTE ON FUNCTION rule_expression.owner_only() TO rule_expression_caller",
    );
    exec(
        &engine,
        "GRANT USAGE ON SEQUENCE rule_expression.owner_sequence TO rule_expression_caller",
    );
    exec(&engine, "SET ROLE rule_expression_caller");
    exec(
        &engine,
        "INSERT INTO rule_expression.routine_owner_event VALUES (2)",
    );
    exec(
        &engine,
        "INSERT INTO rule_expression.nextval_owner_event VALUES (2)",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        strings(
            &engine,
            "SELECT kind || ':' || value::text AS entry FROM rule_expression.action_log ORDER BY kind",
            "entry",
        ),
        ["nextval-owner:1", "routine-owner:101"]
    );
}
