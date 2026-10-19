//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replay `PostgreSQL`'s two-session primary-key lock schedule without unrelated maintenance actors.

use crate::{tests::relation_lock_support::sessions, Engine};

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn inherited_primary_key_locks_match_postgresql(#[case] provider: usize) {
    // The captured schedule has two lock owners. The shared fixture joins automatic ANALYZE before starting either actor.
    let (_directory, engine, peer) = sessions(provider);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/inherited_primary_key_locks_oracle.expected.json"
    ))
    .unwrap();
    assert!(reference["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    for step in reference["cases"].as_array().unwrap() {
        let session = match step["session"].as_str().unwrap() {
            "A" => &engine,
            "B" => &peer,
            other => panic!("unknown lock actor {other}"),
        };
        verify_lock_command(session, step);
    }
}

fn verify_lock_command(engine: &Engine, step: &serde_json::Value) {
    let statement = step["sql"].as_str().unwrap();
    engine.take_sql_notices();
    let mut tags = Vec::new();
    let mut results = Vec::new();
    let outcome = engine.sql_simple_query(statement, &[], |result| {
        assert!(
            result.columns.is_empty() && result.column_types.is_empty() && result.rows.is_empty(),
            "lock command unexpectedly returned rows: {statement}"
        );
        tags.push(result.command_tag.clone());
        results.push(serde_json::json!({"columns": [], "type_oids": [], "rows": []}));
        Ok(())
    });
    let error = outcome.err().map(|error| {
        serde_json::json!({
            "sqlstate": error.sqlstate(),
            "message": error.to_string(),
            "detail": error.detail(),
            "hint": error.hint(),
        })
    });
    assert_eq!(
        serde_json::json!({"command_tags": tags, "error": error, "results": results}),
        serde_json::json!({
            "command_tags": step["command_tags"],
            "error": step["error"],
            "results": step["results"],
        }),
        "lock actor {}: {statement}",
        step["session"]
    );
}
