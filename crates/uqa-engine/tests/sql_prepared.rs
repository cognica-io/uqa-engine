//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PREPARE` / `EXECUTE` / `DEALLOCATE` round-trips.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn prepare_then_execute_returns_query_result() {
    let eng = setup();
    eng.sql("PREPARE q AS SELECT name FROM t WHERE id = $1", &[])
        .unwrap();
    let r = eng.sql("EXECUTE q(2)", &[]).unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("name"), Some(&Value::Str("bob".into())));
}

#[test]
fn deallocate_drops_named_statement() {
    let eng = setup();
    eng.sql("PREPARE qd AS SELECT id FROM t", &[]).unwrap();
    eng.sql("DEALLOCATE qd", &[]).unwrap();
    let err = eng.sql("EXECUTE qd", &[]).unwrap_err();
    assert!(format!("{err:?}").contains("does not exist"));
}
