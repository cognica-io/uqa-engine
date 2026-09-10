//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 asynchronous notification and rewrite-rule action coverage.

#[path = "sql_notifications/cross_process.rs"]
mod cross_process;

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLNotification};
use uqa_sql::ast::ColumnType;

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

    exec(&listener, "LISTEN anchor_channel");
    exec(&listener, "BEGIN");
    exec(&sender, "NOTIFY added_channel, 'before LISTEN command'");
    exec(&listener, "LISTEN added_channel");
    exec(&sender, "NOTIFY added_channel, 'after LISTEN command'");
    exec(&listener, "COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [
            ("added_channel".into(), "before LISTEN command".into()),
            ("added_channel".into(), "after LISTEN command".into())
        ]
    );

    exec(&listener, "UNLISTEN *");

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
fn notification_functions_share_committed_listener_and_queue_state() {
    let directory = TempDir::new().unwrap();
    let root = Engine::open(&directory.path().join("notification_functions.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();

    assert_ne!(listener.backend_process_id(), sender.backend_process_id());
    let backend_pid = exec(&sender, "SELECT pg_backend_pid() AS pid");
    assert_eq!(
        backend_pid.rows[0].get("pid"),
        Some(&Value::Int(i64::from(sender.backend_process_id())))
    );
    assert_eq!(backend_pid.column_types, [Some(ColumnType::Integer)]);

    exec(&listener, "LISTEN first_channel; LISTEN second_channel");
    let initial = exec(&listener, "SELECT pg_listening_channels() AS channel");
    assert_eq!(
        initial
            .rows
            .iter()
            .map(|row| row.get("channel").cloned().unwrap())
            .collect::<Vec<_>>(),
        [
            Value::Str("first_channel".into()),
            Value::Str("second_channel".into())
        ]
    );
    assert_eq!(initial.column_types, [Some(ColumnType::Text)]);

    exec(&listener, "BEGIN; UNLISTEN first_channel");
    exec(
        &sender,
        "SELECT pg_notify('third_channel', 'before LISTEN command')",
    );
    exec(&listener, "LISTEN third_channel");
    let transactional = exec(
        &listener,
        "SELECT * FROM pg_listening_channels() AS channels",
    );
    assert_eq!(
        transactional
            .rows
            .iter()
            .map(|row| row.get("channels").cloned().unwrap())
            .collect::<Vec<_>>(),
        [
            Value::Str("first_channel".into()),
            Value::Str("second_channel".into())
        ]
    );
    exec(
        &sender,
        "SELECT pg_notify('third_channel', 'after LISTEN command')",
    );
    exec(&listener, "COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [
            ("third_channel".into(), "before LISTEN command".into()),
            ("third_channel".into(), "after LISTEN command".into())
        ]
    );
    let committed = exec(&listener, "SELECT pg_listening_channels() AS channel");
    assert_eq!(
        committed
            .rows
            .iter()
            .map(|row| row.get("channel").cloned().unwrap())
            .collect::<Vec<_>>(),
        [
            Value::Str("second_channel".into()),
            Value::Str("third_channel".into())
        ]
    );

    exec(&listener, "LISTEN dynamic_channel");
    let notified = exec(
        &sender,
        "SELECT pg_notify('dynamic_' || 'channel', NULL) AS sent, pg_notify('dynamic_' || 'channel', NULL) IS NULL AS is_null",
    );
    assert_eq!(notified.rows[0].get("sent"), Some(&Value::Void));
    assert_eq!(notified.rows[0].get("is_null"), Some(&Value::Bool(false)));
    assert_eq!(notified.column_types[0], Some(ColumnType::Void));
    let received = listener.take_sql_notifications();
    assert_eq!(
        values(received.clone()),
        [("dynamic_channel".into(), String::new())]
    );
    assert_eq!(received[0].process_id, sender.backend_process_id());
}

#[test]
fn void_is_a_non_null_result_type_with_string_io_casts() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT 'ignored'::void AS value, NULL::void AS null_value, ('ignored'::text)::void AS text_input, ('ignored'::void)::text AS text_value, to_json('ignored'::void)::text AS json_value, pg_typeof('ignored'::void)::text AS type_name, 'ignored'::void IS NULL AS is_null",
    );
    assert_eq!(result.rows[0].get("value"), Some(&Value::Void));
    assert_eq!(result.rows[0].get("null_value"), Some(&Value::Null));
    assert_eq!(result.rows[0].get("text_input"), Some(&Value::Void));
    assert_eq!(
        result.rows[0].get("text_value"),
        Some(&Value::Str(String::new()))
    );
    assert_eq!(
        result.rows[0].get("json_value"),
        Some(&Value::Str("\"\"".into()))
    );
    assert_eq!(
        result.rows[0].get("type_name"),
        Some(&Value::Str("void".into()))
    );
    assert_eq!(result.rows[0].get("is_null"), Some(&Value::Bool(false)));
    assert_eq!(result.column_types[0], Some(ColumnType::Void));

    for (sql, message) in [
        ("SELECT 1::void", "cannot cast type integer to void"),
        (
            "SELECT ('ignored'::void)::integer",
            "cannot cast type void to integer",
        ),
    ] {
        let error = engine
            .sql(sql, &[])
            .expect_err("void casts must follow PostgreSQL's string-category boundary");
        assert_eq!(error.sqlstate(), Some("42846"), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
}

#[test]
fn void_has_no_equality_or_ordering_operators() {
    let engine = Engine::new();
    for sql in [
        "SELECT 'left'::void = 'right'::void",
        "SELECT 'left'::void IS DISTINCT FROM 'right'::void",
        "SELECT 'value'::void BETWEEN 'low'::void AND 'high'::void",
        "SELECT 'value'::void IN ('candidate'::void)",
        "SELECT CASE 'value'::void WHEN 'candidate'::void THEN 1 END",
        "SELECT 'value'::void IN (SELECT 'candidate'::void)",
        "SELECT DISTINCT 'ignored'::void",
        "SELECT DISTINCT ON ('ignored'::void) 1",
        "SELECT 'ignored'::void GROUP BY 1",
        "SELECT 'ignored'::void ORDER BY 1",
        "SELECT count(DISTINCT 'ignored'::void)",
        "SELECT array_agg(1 ORDER BY 'ignored'::void)",
        "SELECT row_number() OVER (PARTITION BY 'ignored'::void)",
        "SELECT row_number() OVER (ORDER BY 'ignored'::void)",
        "SELECT 'left'::void UNION SELECT 'right'::void",
        "SELECT 'left'::void INTERSECT ALL SELECT 'right'::void",
        "SELECT 'left'::void EXCEPT ALL SELECT 'right'::void",
    ] {
        let error = engine
            .sql(sql, &[])
            .expect_err("void has no PostgreSQL equality or ordering operators");
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }

    let union_all = exec(
        &engine,
        "SELECT 'left'::void AS value UNION ALL SELECT 'right'::void",
    );
    assert_eq!(union_all.column_types, [Some(ColumnType::Void)]);
    assert_eq!(union_all.rows.len(), 2);
    assert!(union_all
        .rows
        .iter()
        .all(|row| row.get("value") == Some(&Value::Void)));
}

#[test]
fn void_and_other_pseudo_types_are_not_relation_column_types() {
    let engine = Engine::new();
    for sql in [
        "SELECT '{}'::void[]",
        "CREATE TABLE bad_void_array(v void[])",
    ] {
        let error = engine
            .sql(sql, &[])
            .expect_err("PostgreSQL does not define an array type for void");
        assert_eq!(error.sqlstate(), Some("42704"), "{sql}: {error}");
        assert_eq!(error.to_string(), "type \"void[]\" does not exist", "{sql}");
    }

    for (sql, column, pseudo_type) in [
        ("CREATE TABLE bad_direct(v void)", "v", "void"),
        (
            "CREATE TABLE bad_ctas AS SELECT 'ignored'::void AS v",
            "v",
            "void",
        ),
        (
            "CREATE VIEW bad_view AS SELECT 'ignored'::void AS v",
            "v",
            "void",
        ),
        (
            "CREATE MATERIALIZED VIEW bad_materialized_view AS SELECT 'ignored'::void AS v",
            "v",
            "void",
        ),
        (
            "CREATE FOREIGN TABLE bad_foreign(v void) SERVER absent",
            "v",
            "void",
        ),
        ("CREATE TABLE bad_record(v record)", "v", "record"),
        ("CREATE TABLE bad_record_array(v record[])", "v", "record[]"),
        (
            "CREATE VIEW bad_record_array_view AS SELECT ARRAY[ROW(1)] AS v",
            "v",
            "record[]",
        ),
        (
            "CREATE MATERIALIZED VIEW bad_record_array_materialized_view AS SELECT ARRAY[ROW(1)] AS v",
            "v",
            "record[]",
        ),
        ("CREATE TABLE bad_anyarray(v anyarray)", "v", "anyarray"),
    ] {
        let error = engine
            .sql(sql, &[])
            .expect_err("relation columns must reject PostgreSQL pseudo-types");
        assert_eq!(error.sqlstate(), Some("42P16"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            format!("column \"{column}\" has pseudo-type {pseudo_type}"),
            "{sql}"
        );
    }

    exec(&engine, "CREATE TABLE alter_void_target(v text)");
    for (sql, column) in [
        (
            "ALTER TABLE alter_void_target ADD COLUMN added void",
            "added",
        ),
        (
            "ALTER TABLE alter_void_target ALTER COLUMN v TYPE void",
            "v",
        ),
    ] {
        let error = engine
            .sql(sql, &[])
            .expect_err("ALTER TABLE must reject void relation columns");
        assert_eq!(error.sqlstate(), Some("42P16"));
        assert_eq!(
            error.to_string(),
            format!("column \"{column}\" has pseudo-type void")
        );
    }
}

#[test]
fn notification_functions_enforce_postgresql_arguments_and_report_queue_usage() {
    let directory = TempDir::new().unwrap();
    let root = Engine::open(&directory.path().join("notification_queue.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();

    for (sql, message) in [
        (
            "SELECT pg_notify(NULL, 'payload')",
            "channel name cannot be empty",
        ),
        (
            "SELECT pg_notify('', 'payload')",
            "channel name cannot be empty",
        ),
        (
            &format!("SELECT pg_notify('{}', '')", "c".repeat(64)),
            "channel name too long",
        ),
        (
            &format!("SELECT pg_notify('payload_limit', '{}')", "x".repeat(8_000)),
            "payload string too long",
        ),
    ] {
        let error = sender
            .sql(sql, &[])
            .expect_err("invalid pg_notify arguments must fail");
        assert_eq!(error.sqlstate(), Some("22023"));
        assert_eq!(error.to_string(), message);
    }

    for sql in [
        "SELECT pg_notify('only-one-argument')",
        "SELECT pg_notify(1, 'payload')",
        "SELECT pg_notify(channel => 'named', payload => 'payload')",
        "SELECT pg_backend_pid(extra => 1)",
        "SELECT pg_listening_channels(1)",
        "SELECT pg_notification_queue_usage(1)",
    ] {
        let error = sender
            .sql(sql, &[])
            .expect_err("undefined notification function signatures must fail");
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }

    exec(
        &sender,
        &format!("SELECT pg_notify('{}', '')", "가".repeat(21)),
    );
    let error = sender
        .sql(&format!("SELECT pg_notify('{}', '')", "가".repeat(22)), &[])
        .expect_err("notification channel limits count UTF-8 bytes");
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(error.to_string(), "channel name too long");

    exec(&listener, "LISTEN queue_channel");
    exec(&listener, "BEGIN");
    for payload in ["a".repeat(5_000), "b".repeat(5_000)] {
        exec(
            &sender,
            &format!("SELECT pg_notify('queue_channel', '{payload}')"),
        );
    }
    let used = exec(&sender, "SELECT pg_notification_queue_usage() AS usage");
    assert!(matches!(used.rows[0].get("usage"), Some(Value::Float(value)) if *value > 0.0));
    exec(&listener, "ROLLBACK");
    let empty = exec(&sender, "SELECT pg_notification_queue_usage() AS usage");
    assert_eq!(empty.rows[0].get("usage"), Some(&Value::Float(0.0)));
}

#[test]
fn notification_function_catalog_rows_match_postgresql_18() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT oid, proname, prorettype, proargtypes, proretset, provolatile, proparallel, proisstrict, proleakproof, prosrc, procost, prorows FROM pg_proc WHERE oid IN (2026, 3035, 3036, 3296) ORDER BY oid",
    );
    let expected = [
        (
            2026,
            "pg_backend_pid",
            23,
            Vec::<i64>::new(),
            false,
            "s",
            "r",
            true,
            false,
            "pg_backend_pid",
            1.0,
            0.0,
        ),
        (
            3035,
            "pg_listening_channels",
            25,
            Vec::<i64>::new(),
            true,
            "s",
            "r",
            true,
            false,
            "pg_listening_channels",
            1.0,
            10.0,
        ),
        (
            3036,
            "pg_notify",
            2278,
            vec![25, 25],
            false,
            "v",
            "r",
            false,
            false,
            "pg_notify",
            1.0,
            0.0,
        ),
        (
            3296,
            "pg_notification_queue_usage",
            701,
            Vec::<i64>::new(),
            false,
            "v",
            "r",
            true,
            false,
            "pg_notification_queue_usage",
            1.0,
            0.0,
        ),
    ];
    assert_eq!(result.rows.len(), expected.len());
    for (row, expected) in result.rows.iter().zip(expected) {
        assert_eq!(row.get("oid"), Some(&Value::Int(expected.0)));
        assert_eq!(row.get("proname"), Some(&Value::Str(expected.1.into())));
        assert_eq!(row.get("prorettype"), Some(&Value::Int(expected.2)));
        assert_eq!(
            row.get("proargtypes"),
            Some(&Value::List(
                expected.3.into_iter().map(Value::Int).collect()
            ))
        );
        assert_eq!(row.get("proretset"), Some(&Value::Bool(expected.4)));
        assert_eq!(row.get("provolatile"), Some(&Value::Str(expected.5.into())));
        assert_eq!(row.get("proparallel"), Some(&Value::Str(expected.6.into())));
        assert_eq!(row.get("proisstrict"), Some(&Value::Bool(expected.7)));
        assert_eq!(row.get("proleakproof"), Some(&Value::Bool(expected.8)));
        assert_eq!(row.get("prosrc"), Some(&Value::Str(expected.9.into())));
        assert_eq!(row.get("procost"), Some(&Value::Float(expected.10)));
        assert_eq!(row.get("prorows"), Some(&Value::Float(expected.11)));
    }
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
    assert_eq!(notifications[0].process_id, sender.backend_process_id());
    assert_ne!(listener.backend_process_id(), sender.backend_process_id());
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
