//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporary namespace cleanup follows dependencies and the original transaction owner.

use super::{int, run, text};
use serde_json::Value as Json;
use uqa_engine::Engine;

fn reference(cleanup: &str) -> Json {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/temporary_catalog_lifetime.expected.json"
    ))
    .unwrap();
    assert!(reference["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["cleanup"] == cleanup)
        .unwrap()
        .clone()
}

fn observe(engine: &Engine, reference: &Json) -> Json {
    let result = engine
        .sql(reference["observe"].as_str().unwrap(), &[])
        .unwrap();
    match result.value_at(0, 0) {
        Some(uqa_core::Value::Json(value) | uqa_core::Value::JsonB(value)) => {
            serde_json::from_str(value).unwrap()
        }
        other => panic!("JSON catalog observation: {other:?}"),
    }
}

#[rstest::rstest]
fn discard_removes_namespace_members_and_dependents_transactionally(
    #[values(0, 1, 2, 3)] provider: usize,
    #[values("DISCARD TEMP", "DISCARD TEMP READ ONLY", "DISCARD ALL")] cleanup: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-discard.db");
    let engine = super::types::open(provider, &path);
    let reference = reference(cleanup);
    run(&engine, reference["setup"].as_str().unwrap());
    let namespace = int(&engine, "SELECT pg_my_temp_schema()::bigint");
    assert_eq!(observe(&engine, &reference), reference["observations"][0]);
    if cleanup.starts_with("DISCARD TEMP") {
        let read_only = cleanup.ends_with("READ ONLY");
        run(
            &engine,
            if read_only {
                "BEGIN READ ONLY"
            } else {
                "BEGIN"
            },
        );
        run(&engine, "DISCARD TEMP");
        assert_eq!(observe(&engine, &reference), reference["observations"][1]);
        assert_eq!(
            text(&engine, "SHOW transaction_read_only").as_deref(),
            Some(if read_only { "on" } else { "off" })
        );
        run(&engine, "ROLLBACK");
        assert_eq!(observe(&engine, &reference), reference["observations"][2]);
        run(&engine, "DISCARD TEMP");
    } else {
        run(&engine, "DISCARD ALL");
    }
    assert_eq!(observe(&engine, &reference), reference["after_close"][0]);
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        namespace
    );
    engine.close().unwrap();
    engine.close().unwrap();
    drop(engine);
    if provider != 0 {
        let reopened = super::types::open(provider, &path);
        assert_eq!(observe(&reopened, &reference), reference["after_close"][0]);
    }
}

#[rstest::rstest]
fn close_and_drop_finish_temporary_catalog_cleanup(
    #[values(1, 2, 3)] provider: usize,
    #[values("close", "drop", "cancelled", "terminated", "failed", "read-only")] exit: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-close.db");
    let engine = super::types::open(provider, &path);
    let reference = reference("close");
    run(&engine, reference["setup"].as_str().unwrap());
    assert_eq!(observe(&engine, &reference), reference["observations"][0]);
    let peer = engine.new_session().unwrap();
    run(&peer, "CREATE DOMAIN pg_temp.sibling_domain AS integer");
    match exit {
        "cancelled" => engine.cancel(),
        "terminated" => engine.cancellation_token().cancel_with(uqa_core::CancellationReason::IdleSessionTimeout),
        "failed" => {
            run(&engine, "BEGIN; UPDATE lifetime_test.dependent SET n=99");
            assert!(engine.sql("SELECT 1/0", &[]).is_err());
        }
        "read-only" => run(&engine, "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY, DEFERRABLE"),
        _ => {}
    }
    let token = engine.cancellation_token();
    if exit != "drop" {
        engine.close().unwrap();
        engine.close().unwrap();
    }
    drop(engine);
    if matches!(exit, "cancelled" | "terminated") {
        assert!(token.check().is_err());
    }
    assert_eq!(observe(&peer, &reference), reference["after_close"][0]);
    assert_eq!(int(&peer, "SELECT count(*) FROM pg_type WHERE typname='sibling_domain' AND typnamespace=pg_my_temp_schema()"), 1);
    drop(peer);
    let reopened = super::types::open(provider, &path);
    assert_eq!(observe(&reopened, &reference), reference["after_close"][0]);
    assert_eq!(
        int(
            &reopened,
            "SELECT count(*) FROM pg_type WHERE typname='sibling_domain'"
        ),
        0
    );
}

#[rstest::rstest]
fn session_timeouts_finish_temporary_catalog_cleanup_before_explicit_close(
    #[values(1, 2, 3)] provider: usize,
    #[values(
        "idle_session_timeout",
        "idle_in_transaction_session_timeout",
        "transaction_timeout",
        "transaction_timeout busy"
    )]
    cleanup: &str,
) {
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-timeout.db");
    let engine = super::types::open(provider, &path);
    let reference = reference(cleanup);
    run(&engine, reference["setup"].as_str().unwrap());
    assert_eq!(observe(&engine, &reference), reference["observations"][0]);
    let peer = engine.new_session().unwrap();
    run(&peer, "CREATE DOMAIN pg_temp.sibling_domain AS integer");
    run(&engine, reference["arm"].as_str().unwrap());
    if reference["busy"].as_bool().unwrap() {
        let failure = engine.sql("SELECT pg_sleep(60)", &[]).err().unwrap();
        assert_eq!(failure.sqlstate(), reference["sqlstate"].as_str());
    }
    let started = Instant::now();
    loop {
        let after = observe(&peer, &reference);
        if engine.session_termination().is_some() && after == reference["after_termination"][0] {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{cleanup}: termination did not clean the temporary catalog: {after}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..2 {
        assert_eq!(
            engine.sql("SELECT 1", &[]).err().unwrap().sqlstate(),
            reference["sqlstate"].as_str()
        );
    }
    assert_eq!(int(&peer, "SELECT count(*) FROM pg_type WHERE typname='sibling_domain' AND typnamespace=pg_my_temp_schema()"), 1);
    engine.close().unwrap();
    engine.close().unwrap();
    drop(engine);
    drop(peer);
    let reopened = super::types::open(provider, &path);
    assert_eq!(observe(&reopened, &reference), reference["after_close"][0]);
}
