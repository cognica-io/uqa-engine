//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `EXPLAIN SELECT ...` returns a single-column `plan` table mirroring
//! Python's `_explain_plan` output shape.

use uqa_engine::Engine;

#[test]
fn explain_returns_plan_rows() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)", &[])
        .unwrap();
    let r = eng
        .sql("EXPLAIN SELECT id FROM t WHERE n > 1 LIMIT 10", &[])
        .unwrap();
    assert_eq!(r.columns, vec!["plan".to_string()]);
    assert!(!r.rows.is_empty());
    // The plan must mention the from clause and limit, at minimum.
    let blob = r
        .rows
        .iter()
        .filter_map(|row| match row.get("plan") {
            Some(uqa_core::Value::Str(s)) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(blob.contains("Select"));
    assert!(blob.contains("limit=10"));
}
