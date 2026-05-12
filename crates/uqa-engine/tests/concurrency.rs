//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concurrent-read smoke test: spin up N reader threads against a
//! shared `Engine`, all running `text_match` queries simultaneously.
//! Asserts every reader observes the same result set without dead-lock
//! or corruption.

use std::fmt::Write as _;
use std::sync::Arc;
use std::thread;

use uqa_engine::Engine;

const READERS: usize = 16;
const ITERATIONS_PER_READER: usize = 32;

fn build_engine() -> Arc<Engine> {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    let mut sql = String::from("INSERT INTO docs (id, body) VALUES ");
    for i in 0..200 {
        if i > 0 {
            sql.push_str(", ");
        }
        let token = if i % 5 == 0 { "alpha" } else { "bravo" };
        write!(sql, "({i}, 'doc {i} body has {token} content')").unwrap();
    }
    engine.sql(&sql, &[]).unwrap();
    Arc::new(engine)
}

#[test]
fn concurrent_text_match_readers_see_consistent_results() {
    let engine = build_engine();
    let baseline = engine
        .sql(
            "SELECT id FROM docs WHERE text_match(body, 'alpha') ORDER BY id",
            &[],
        )
        .unwrap();
    let baseline_ids: Vec<i64> = baseline
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(uqa_core::Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert!(!baseline_ids.is_empty());

    let mut handles = Vec::with_capacity(READERS);
    for _ in 0..READERS {
        let engine = engine.clone();
        let expected = baseline_ids.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..ITERATIONS_PER_READER {
                let r = engine
                    .sql(
                        "SELECT id FROM docs WHERE text_match(body, 'alpha') ORDER BY id",
                        &[],
                    )
                    .expect("sql ok");
                let ids: Vec<i64> = r
                    .rows
                    .iter()
                    .filter_map(|row| match row.get("id") {
                        Some(uqa_core::Value::Int(n)) => Some(*n),
                        _ => None,
                    })
                    .collect();
                assert_eq!(ids, expected, "reader observed divergent results");
            }
        }));
    }
    for h in handles {
        h.join().expect("reader thread panicked");
    }
}
