//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 asynchronous notification and rewrite-rule action coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLNotification};

fn exec(engine: &Engine, sql: &str) -> uqa_engine::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn values(notifications: Vec<SQLNotification>) -> Vec<(String, String)> {
    notifications
        .into_iter()
        .map(|notification| (notification.channel, notification.payload))
        .collect()
}

#[test]
fn notifications_follow_outer_transaction_and_savepoint_boundaries() {
    let directory = TempDir::new().unwrap();
    let root = Engine::open(&directory.path().join("notifications.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();

    exec(&listener, "LISTEN events");
    exec(
        &sender,
        "BEGIN; NOTIFY events, 'a'; NOTIFY events, 'a'; NOTIFY events, 'b'",
    );
    assert!(listener.take_sql_notifications().is_empty());
    exec(&sender, "COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("events".into(), "a".into()), ("events".into(), "b".into())]
    );

    exec(&listener, "BEGIN");
    exec(&sender, "NOTIFY events, 'deferred'");
    assert!(listener.take_sql_notifications().is_empty());
    exec(&listener, "ROLLBACK");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("events".into(), "deferred".into())]
    );

    exec(
        &sender,
        "BEGIN; NOTIFY events, 'outer'; SAVEPOINT notification_point; NOTIFY events, 'inner'; ROLLBACK TO SAVEPOINT notification_point; COMMIT",
    );
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("events".into(), "outer".into())]
    );

    exec(
        &sender,
        "BEGIN; NOTIFY events, 'before error'; SAVEPOINT error_point; NOTIFY events, 'failed statement'",
    );
    let error = sender
        .sql("SELECT 1 / 0", &[])
        .expect_err("division by zero must abort the statement");
    assert_eq!(error.sqlstate(), Some("22012"));
    exec(&sender, "ROLLBACK TO SAVEPOINT error_point; COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("events".into(), "before error".into())]
    );

    exec(&sender, "BEGIN; NOTIFY events, 'rolled back'; ROLLBACK");
    assert!(listener.take_sql_notifications().is_empty());

    let error = sender
        .sql("NOTIFY events, 'implicit rollback'; SELECT 1 / 0", &[])
        .expect_err("a later statement must roll back the implicit transaction");
    assert_eq!(error.sqlstate(), Some("22012"));
    assert!(listener.take_sql_notifications().is_empty());

    exec(
        &sender,
        "CREATE FUNCTION nested_notifications() RETURNS INTEGER AS $$ BEGIN NOTIFY events, 'nested duplicate'; BEGIN NOTIFY events, 'nested duplicate'; NOTIFY events, 'nested discarded'; PERFORM 1 / 0; EXCEPTION WHEN division_by_zero THEN NULL; END; NOTIFY events, 'nested kept'; RETURN 1; END $$ LANGUAGE plpgsql; SELECT nested_notifications()",
    );
    assert_eq!(
        values(listener.take_sql_notifications()),
        [
            ("events".into(), "nested duplicate".into()),
            ("events".into(), "nested kept".into())
        ]
    );
}

#[test]
fn listener_changes_are_transactional_and_discard_all_unsubscribes() {
    let directory = TempDir::new().unwrap();
    let root = Engine::open(&directory.path().join("listener_state.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();

    exec(&listener, "BEGIN; LISTEN rolled_back; ROLLBACK");
    exec(&sender, "NOTIFY rolled_back, 'absent'");
    assert!(listener.take_sql_notifications().is_empty());

    exec(&listener, "LISTEN retained");
    exec(&listener, "BEGIN; UNLISTEN retained; ROLLBACK");
    exec(&sender, "NOTIFY retained, 'present'");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("retained".into(), "present".into())]
    );

    exec(
        &listener,
        "BEGIN; LISTEN self_channel; NOTIFY self_channel, 'self'; COMMIT",
    );
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("self_channel".into(), "self".into())]
    );

    exec(
        &listener,
        "BEGIN; UNLISTEN self_channel; NOTIFY self_channel, 'removed'; COMMIT",
    );
    assert!(listener.take_sql_notifications().is_empty());

    exec(
        &listener,
        "BEGIN READ ONLY; LISTEN readonly_channel; NOTIFY readonly_channel, 'allowed'; COMMIT",
    );
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("readonly_channel".into(), "allowed".into())]
    );

    exec(&listener, "UNLISTEN *");
    exec(&sender, "NOTIFY retained, 'unlistened from all'");
    assert!(listener.take_sql_notifications().is_empty());

    exec(&listener, "LISTEN \"*\"");
    exec(&sender, "NOTIFY \"*\", 'quoted star'");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("*".into(), "quoted star".into())]
    );
    exec(&listener, "UNLISTEN \"*\"");
    exec(&sender, "NOTIFY \"*\", 'removed quoted star'");
    assert!(listener.take_sql_notifications().is_empty());

    exec(&listener, "LISTEN retained");
    exec(&listener, "DISCARD ALL");
    exec(&sender, "NOTIFY retained, 'discarded'");
    assert!(listener.take_sql_notifications().is_empty());
}

#[test]
fn listener_commit_uses_the_final_subscription_for_notifications_received_in_flight() {
    let directory = TempDir::new().unwrap();
    let root = Engine::open(&directory.path().join("listener_commit_boundary.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();

    exec(&listener, "LISTEN existing_channel");
    exec(&sender, "NOTIFY existing_channel, 'before transaction'");
    exec(&listener, "BEGIN; UNLISTEN existing_channel");
    exec(
        &sender,
        "NOTIFY existing_channel, 'during committed unlisten'",
    );
    exec(&listener, "COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("existing_channel".into(), "before transaction".into())]
    );

    exec(&listener, "BEGIN; LISTEN new_channel");
    exec(&sender, "NOTIFY new_channel, 'before listen commit'");
    exec(&listener, "COMMIT");
    assert!(listener.take_sql_notifications().is_empty());
    exec(&sender, "NOTIFY new_channel, 'after listen commit'");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("new_channel".into(), "after listen commit".into())]
    );

    exec(&listener, "LISTEN rollback_channel");
    exec(&listener, "BEGIN; UNLISTEN rollback_channel");
    exec(
        &sender,
        "NOTIFY rollback_channel, 'during rolled back unlisten'",
    );
    exec(&listener, "ROLLBACK");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [(
            "rollback_channel".into(),
            "during rolled back unlisten".into()
        )]
    );
}

#[test]
fn independently_opened_engines_share_committed_notifications() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("shared_notifications.db");
    let listener = Engine::open(&path).unwrap();
    let sender = Engine::open(&path).unwrap();

    exec(&listener, "LISTEN shared_channel");
    exec(&sender, "NOTIFY shared_channel, 'from sibling'");
    let notifications = listener.take_sql_notifications();
    assert_eq!(
        values(notifications.clone()),
        [("shared_channel".into(), "from sibling".into())]
    );
    assert_ne!(notifications[0].sender_session_id, 0);
}

#[test]
fn notify_enforces_postgresql_payload_limit() {
    let engine = Engine::new();
    exec(&engine, "LISTEN payload_limit");
    exec(&engine, "NOTIFY payload_limit");
    assert_eq!(engine.take_sql_notifications()[0].payload, "");
    exec(
        &engine,
        &format!("NOTIFY payload_limit, '{}'", "x".repeat(7_999)),
    );
    assert_eq!(engine.take_sql_notifications()[0].payload.len(), 7_999);

    let error = engine
        .sql(
            &format!("NOTIFY payload_limit, '{}'", "x".repeat(8_000)),
            &[],
        )
        .expect_err("8,000-byte payload must fail");
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(error.to_string(), "payload string too long");
    assert!(engine.take_sql_notifications().is_empty());
}

#[test]
fn notify_rule_actions_execute_once_per_statement_and_rollback_atomically() {
    let engine = Engine::new();
    exec(&engine, "LISTEN rule_events");
    exec(
        &engine,
        "CREATE TABLE rule_notify_items(id INTEGER); CREATE TABLE rule_notify_empty(id INTEGER); CREATE RULE insert_notification AS ON INSERT TO rule_notify_items DO ALSO NOTIFY rule_events, 'inserted'; CREATE RULE update_notification AS ON UPDATE TO rule_notify_items DO ALSO NOTIFY rule_events, 'updated'; CREATE RULE delete_notification AS ON DELETE TO rule_notify_items DO ALSO NOTIFY rule_events, 'deleted'",
    );

    exec(&engine, "INSERT INTO rule_notify_items VALUES (1), (2)");
    exec(
        &engine,
        "INSERT INTO rule_notify_items SELECT id FROM rule_notify_empty",
    );
    exec(&engine, "UPDATE rule_notify_items SET id = id WHERE false");
    exec(&engine, "DELETE FROM rule_notify_items WHERE false");
    assert_eq!(
        values(engine.take_sql_notifications()),
        [
            ("rule_events".into(), "inserted".into()),
            ("rule_events".into(), "inserted".into()),
            ("rule_events".into(), "updated".into()),
            ("rule_events".into(), "deleted".into()),
        ]
    );

    exec(
        &engine,
        "BEGIN; INSERT INTO rule_notify_items VALUES (3); ROLLBACK",
    );
    assert!(engine.take_sql_notifications().is_empty());

    let definition = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'insert_notification'",
    );
    assert!(matches!(
        definition.rows[0].get("definition"),
        Some(Value::Str(text)) if text.contains("NOTIFY rule_events, 'inserted'")
    ));

    let error = engine
        .sql(
            "CREATE RULE conditional_notification AS ON INSERT TO rule_notify_items WHERE NEW.id > 0 DO ALSO NOTIFY rule_events, 'conditional'",
            &[],
        )
        .expect_err("conditional NOTIFY action must fail");
    assert_eq!(error.sqlstate(), Some("42P17"));
    assert_eq!(
        error.to_string(),
        "rules with WHERE conditions can only have SELECT, INSERT, UPDATE, or DELETE actions"
    );
}

#[test]
fn instead_notify_rule_suppresses_the_original_command() {
    let engine = Engine::new();
    exec(&engine, "LISTEN instead_events");
    exec(
        &engine,
        "CREATE TABLE instead_notify_items(id INTEGER); CREATE RULE replace_insert AS ON INSERT TO instead_notify_items DO INSTEAD NOTIFY instead_events, 'replaced'",
    );
    let result = exec(&engine, "INSERT INTO instead_notify_items VALUES (1), (2)");
    assert_eq!(result.affected_rows, 0);
    assert!(exec(&engine, "SELECT id FROM instead_notify_items")
        .rows
        .is_empty());
    assert_eq!(
        values(engine.take_sql_notifications()),
        [("instead_events".into(), "replaced".into())]
    );
}

#[test]
fn notify_rule_action_survives_durable_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("notify_rule_reopen.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE persistent_notify_items(id INTEGER); CREATE RULE persistent_notification AS ON INSERT TO persistent_notify_items DO ALSO NOTIFY persistent_rule_events, 'reopened'",
        );
    }

    let reopened = Engine::open(&path).unwrap();
    exec(&reopened, "LISTEN persistent_rule_events");
    exec(&reopened, "INSERT INTO persistent_notify_items VALUES (1)");
    assert_eq!(
        values(reopened.take_sql_notifications()),
        [("persistent_rule_events".into(), "reopened".into())]
    );
    let definition = exec(
        &reopened,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'persistent_notification'",
    );
    assert!(matches!(
        definition.rows[0].get("definition"),
        Some(Value::Str(text)) if text.contains("NOTIFY persistent_rule_events, 'reopened'")
    ));
}
