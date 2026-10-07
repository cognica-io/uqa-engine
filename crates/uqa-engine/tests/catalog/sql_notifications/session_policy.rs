//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stateless-host notification policy through the real SQL/session boundaries.

use super::{exec, values};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLError;

fn rejected(error: &SQLError) {
    assert!(
        matches!(error, SQLError::NotificationRequiresSubscription),
        "{error:?}"
    );
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(error.code(), Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"));
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    exec(engine, sql).value_at(0, 0).unwrap().clone()
}

#[test]
fn notification_policy_rejects_direct_and_cached_commands_but_not_payload_text() {
    let engine = Engine::new();
    exec(&engine, "UNLISTEN *");
    engine.require_notification_subscriptions().unwrap();
    for sql in [
        "LISTEN events",
        "/* prefix */ LiStEn \"UNLISTEN\"",
        "UNLISTEN events",
        "UNLISTEN *",
    ] {
        rejected(&engine.sql(sql, &[]).unwrap_err());
        rejected(
            &engine
                .sql_cursor(sql, &[])
                .err()
                .expect("cursor must reject the command"),
        );
        rejected(
            &engine
                .sql_columnar(sql, &[], |_| panic!("no result may be delivered"))
                .unwrap_err(),
        );
    }
    assert_eq!(
        exec(&engine, "SELECT * FROM pg_listening_channels()")
            .rows
            .len(),
        0
    );
    assert_eq!(
        scalar(&engine, "SELECT 'LISTEN events; UNLISTEN *'"),
        Value::Str("LISTEN events; UNLISTEN *".into())
    );
    exec(
        &engine,
        "NOTIFY events, 'LISTEN'; SELECT pg_notify('events', 'UNLISTEN *')",
    );
}

#[test]
fn notification_policy_admits_whole_messages_and_api_batches_before_sequence_effects() {
    let engine = Engine::new();
    exec(&engine, "CREATE SEQUENCE admission_counter");
    engine.require_notification_subscriptions().unwrap();
    rejected(
        &engine
            .sql(
                "SELECT nextval('admission_counter'); COMMIT; LISTEN events",
                &[],
            )
            .unwrap_err(),
    );
    rejected(
        &engine
            .sql_batch(&[
                ("SELECT nextval('admission_counter')", &[]),
                ("UNLISTEN *", &[]),
            ])
            .unwrap_err(),
    );
    let mut delivered = 0;
    rejected(
        &engine
            .sql_simple_query(
                "SELECT nextval('admission_counter'); LISTEN events",
                &[],
                |_| {
                    delivered += 1;
                    Ok(())
                },
            )
            .unwrap_err(),
    );
    assert_eq!(delivered, 0);
    assert_eq!(
        scalar(&engine, "SELECT nextval('admission_counter')"),
        Value::Int(1)
    );
}

#[test]
fn notification_policy_rolls_back_nested_static_dynamic_and_rethrown_commands() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE effects (id INTEGER)");
    engine.require_notification_subscriptions().unwrap();
    for body in [
        "LISTEN events;",
        "UNLISTEN events;",
        "UNLISTEN *;",
        "EXECUTE 'LIS' || 'TEN events';",
        "BEGIN LISTEN events; EXCEPTION WHEN feature_not_supported THEN RAISE; END;",
        "BEGIN UNLISTEN *; EXCEPTION WHEN OTHERS THEN BEGIN RAISE; EXCEPTION WHEN OTHERS THEN RAISE; END; END;",
    ] {
        let sql = format!("DO $$ BEGIN INSERT INTO effects VALUES (1); NOTIFY events, 'rolled back'; {body} END $$");
        rejected(&engine.sql(&sql, &[]).unwrap_err());
        assert_eq!(scalar(&engine, "SELECT count(*) FROM effects"), Value::Int(0));
        assert_eq!(exec(&engine, "SELECT * FROM pg_listening_channels()").rows.len(), 0);
    }
    // A deliberate SQL exception handler retains normal subtransaction semantics.
    exec(&engine, "DO $$ BEGIN BEGIN INSERT INTO effects VALUES (1); LISTEN events; EXCEPTION WHEN feature_not_supported THEN INSERT INTO effects VALUES (2); END; END $$");
    assert_eq!(scalar(&engine, "SELECT id FROM effects"), Value::Int(2));
}

#[test]
fn notification_policy_covers_routines_prepared_queries_triggers_and_portals() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE effects (id INTEGER); CREATE FUNCTION listener_function() RETURNS INTEGER LANGUAGE plpgsql AS $$ BEGIN LISTEN events; RETURN 1; END $$; PREPARE invoke_listener AS SELECT listener_function(); CREATE FUNCTION sql_listener() RETURNS void LANGUAGE SQL AS $$ UNLISTEN * $$; CREATE FUNCTION listener_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NOTIFY events, 'rolled back'; LISTEN events; RETURN NEW; END $$; CREATE TRIGGER listener_effect BEFORE INSERT ON effects FOR EACH ROW EXECUTE FUNCTION listener_trigger()");
    // Retain an optimized query before the host policy changes.
    assert_eq!(scalar(&engine, "SELECT listener_function()"), Value::Int(1));
    exec(&engine, "UNLISTEN *");
    engine.require_notification_subscriptions().unwrap();
    for sql in [
        "SELECT listener_function()",
        "EXECUTE invoke_listener",
        "SELECT sql_listener()",
        "INSERT INTO effects VALUES (1)",
    ] {
        rejected(&engine.sql(sql, &[]).unwrap_err());
    }
    for sql in ["SELECT listener_function()", "SELECT sql_listener()"] {
        rejected(
            &engine
                .sql_cursor(sql, &[])
                .err()
                .expect("nested cursor must reject listening"),
        );
    }
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM effects"),
        Value::Int(0)
    );
    exec(
        &engine,
        "BEGIN; DECLARE listener_cursor CURSOR FOR SELECT listener_function()",
    );
    rejected(
        &engine
            .sql("FETCH ALL FROM listener_cursor", &[])
            .unwrap_err(),
    );
    exec(&engine, "ROLLBACK");
}

#[test]
fn notification_policy_keeps_publication_deduplication_and_rollback_transactional() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("policy.db")).unwrap();
    let listener = root.new_session().unwrap();
    let sender = root.new_session().unwrap();
    exec(&listener, "LISTEN events");
    sender.require_notification_subscriptions().unwrap();
    exec(&sender, "BEGIN; NOTIFY events, 'one'; SELECT pg_notify('events', 'one'); SELECT pg_notify('events', 'two')");
    assert_eq!(listener.take_sql_notifications().len(), 0);
    exec(&sender, "COMMIT");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [
            ("events".into(), "one".into()),
            ("events".into(), "two".into())
        ]
    );
    exec(&sender, "BEGIN; NOTIFY events, 'discarded'");
    rejected(
        &sender
            .sql("DO $$ BEGIN UNLISTEN *; END $$", &[])
            .unwrap_err(),
    );
    assert_eq!(
        sender.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    exec(&sender, "ROLLBACK");
    assert_eq!(listener.take_sql_notifications().len(), 0);
    rejected(
        &sender
            .sql_batch(&[
                ("NOTIFY events, 'discarded batch'", &[]),
                ("DO $$ BEGIN LISTEN events; END $$", &[]),
            ])
            .unwrap_err(),
    );
    assert_eq!(listener.take_sql_notifications().len(), 0);
    exec(&sender, "NOTIFY events, 'retained'");
    assert_eq!(
        values(listener.take_sql_notifications()),
        [("events".into(), "retained".into())]
    );
}

#[test]
fn notification_policy_cannot_strand_listeners_or_be_undone_by_rollback_or_discard() {
    let engine = Engine::new();
    exec(&engine, "LISTEN events");
    assert_eq!(
        engine
            .require_notification_subscriptions()
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
    exec(&engine, "NOTIFY events, 'still registered'");
    assert_eq!(
        values(engine.take_sql_notifications()),
        [("events".into(), "still registered".into())]
    );
    exec(&engine, "UNLISTEN *");
    exec(&engine, "BEGIN; LISTEN pending");
    assert_eq!(
        engine
            .require_notification_subscriptions()
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
    exec(&engine, "ROLLBACK");
    engine.require_notification_subscriptions().unwrap();
    exec(&engine, "BEGIN; SAVEPOINT nested");
    rejected(&engine.sql("LISTEN events", &[]).unwrap_err());
    exec(&engine, "ROLLBACK TO nested; ROLLBACK; DISCARD ALL");
    engine.require_notification_subscriptions().unwrap();
    rejected(&engine.sql("LISTEN events", &[]).unwrap_err());
}

#[test]
fn notification_policy_is_inherited_by_new_sessions_without_changing_existing_peers() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("policy_sessions.db")).unwrap();
    let peer = root.new_session().unwrap();
    exec(&root, "CREATE ROLE notification_user LOGIN");
    root.require_notification_subscriptions().unwrap();
    let child = root.new_session().unwrap();
    let authenticated = root.new_session_for_user("notification_user").unwrap();
    rejected(&child.sql("LISTEN events", &[]).unwrap_err());
    rejected(&authenticated.sql("UNLISTEN *", &[]).unwrap_err());
    rejected(
        &child
            .new_session()
            .unwrap()
            .sql("LISTEN events", &[])
            .unwrap_err(),
    );
    exec(&peer, "LISTEN events; NOTIFY events, 'independent'");
    assert_eq!(
        values(peer.take_sql_notifications()),
        [("events".into(), "independent".into())]
    );
}

#[test]
fn notification_policy_api_batch_rejection_keeps_the_callers_transaction() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE effects (id INTEGER)");
    engine.require_notification_subscriptions().unwrap();
    exec(&engine, "BEGIN; INSERT INTO effects VALUES (7)");
    for rejected_sql in ["LISTEN events", "DO $$ BEGIN EXECUTE 'UNLISTEN *'; END $$"] {
        rejected(
            &engine
                .sql_batch(&[("INSERT INTO effects VALUES (8)", &[]), (rejected_sql, &[])])
                .unwrap_err(),
        );
        assert_eq!(
            scalar(&engine, "SELECT count(*) FROM effects"),
            Value::Int(1)
        );
        assert_eq!(scalar(&engine, "SELECT id FROM effects"), Value::Int(7));
    }
    exec(&engine, "COMMIT");
    assert_eq!(scalar(&engine, "SELECT id FROM effects"), Value::Int(7));
}

#[test]
fn notification_policy_does_not_restrict_independently_owned_handles() {
    let engine = Engine::new();
    engine.require_notification_subscriptions().unwrap();
    let handle = engine
        .subscribe_notifications(
            &["events"],
            uqa_engine::NotificationSubscriptionOptions {
                max_active_subscriptions: 1,
                max_channels: 1,
                max_queued_notifications: 1,
                max_queued_bytes: 4096,
                max_registry_entries_per_poll: 1,
            },
        )
        .unwrap();
    exec(&engine, "NOTIFY events, 'retained'");
    let uqa_engine::NotificationWait::Event(
        uqa_core::notifications::NotificationEvent::Notification { notification, .. },
    ) = handle.wait(std::time::Duration::from_secs(5)).unwrap()
    else {
        panic!("independent subscription must receive the publication")
    };
    assert_eq!(notification.channel, "events");
    assert_eq!(notification.payload, "retained");
    handle.close();
}

#[test]
fn notification_policy_preserves_postgresql_nested_exception_diagnostics() {
    // Independently executed on PostgreSQL 18.4; this SQLSTATE/message pair is unchanged by host policy.
    let engine = Engine::new();
    exec(&engine, "CREATE FUNCTION reraise() RETURNS text LANGUAGE plpgsql AS $$ BEGIN BEGIN PERFORM 1 / 0; EXCEPTION WHEN division_by_zero THEN BEGIN RAISE; EXCEPTION WHEN division_by_zero THEN RAISE; END; END; EXCEPTION WHEN division_by_zero THEN RETURN SQLSTATE || ':' || SQLERRM; END $$");
    assert_eq!(
        scalar(&engine, "SELECT reraise()"),
        Value::Str("22012:division by zero".into())
    );
    engine.require_notification_subscriptions().unwrap();
    assert_eq!(
        scalar(&engine, "SELECT reraise()"),
        Value::Str("22012:division by zero".into())
    );
}
