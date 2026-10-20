//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public rewrite-rule candidate lifetime under the query memory allowance.

use super::*;

#[test]
fn suppressed_rule_updates_do_not_retain_complete_candidate_documents() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE suppressed_events(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO suppressed_events SELECT n, repeat('x',32768) FROM generate_series(1,512) AS s(n);
        CREATE RULE suppress_update AS ON UPDATE TO suppressed_events DO INSTEAD NOTHING;
        CREATE VIEW suppressed_view AS SELECT id, body FROM suppressed_events;
        CREATE RULE suppress_view_update AS ON UPDATE TO suppressed_view DO INSTEAD NOTHING");
    engine.set_query_memory_limit(Some(64 * 1024));
    for sql in [
        "UPDATE suppressed_events SET body=body || 'changed'",
        "UPDATE suppressed_events AS e SET body=e.body || s.suffix FROM (VALUES ('changed')) AS s(suffix)",
        "UPDATE suppressed_view SET body=body || 'changed'",
    ] {
        // Includes analysis and every candidate image; existing table storage is outside this query's allocation interval.
        let allocated = allocation_counter::measure(|| {
            exec(&engine, sql);
        });
        assert!(allocated.bytes_max < 2 * 1024 * 1024, "{sql}: {allocated:?}");
    }
    let unchanged = exec(
        &engine,
        "SELECT count(*), min(length(body)), max(length(body)) FROM suppressed_events",
    );
    assert_eq!(unchanged.value_at(0, 0), Some(&Value::Int(512)));
    assert_eq!(unchanged.value_at(0, 1), Some(&Value::Int(32768)));
    assert_eq!(unchanged.value_at(0, 2), Some(&Value::Int(32768)));
}
