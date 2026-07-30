//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `EXPLAIN SELECT ...` returns a single-column `plan` table mirroring
//! the canonical UQA implementation's `_explain_plan` output shape.

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

#[test]
fn explain_analyze_executes_the_owned_plan_and_reports_runtime_counts() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, n) VALUES (1, 10), (2, 20)", &[])
        .unwrap();

    let result = eng
        .sql("EXPLAIN ANALYZE SELECT id FROM t WHERE n >= 10", &[])
        .unwrap();
    let text = result
        .rows
        .iter()
        .filter_map(|row| match row.get("plan") {
            Some(uqa_core::Value::Str(value)) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("actual_rows=2"), "{text}");
    assert!(text.contains("execution_time_ms="), "{text}");
}

#[test]
fn explain_json_is_structured_and_analyze_is_not_a_second_dispatch_path() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();

    let result = eng
        .sql(
            "EXPLAIN (ANALYZE true, VERBOSE true, FORMAT JSON) SELECT id FROM t",
            &[],
        )
        .unwrap();
    let Some(uqa_core::Value::Str(payload)) = result.rows[0].get("plan") else {
        panic!("JSON EXPLAIN must return a string payload");
    };
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(payload["Analyze"], true);
    assert_eq!(payload["Actual Rows"], 1);
    assert!(payload["Plan"]
        .as_array()
        .is_some_and(|lines| lines.iter().any(|line| line
            .as_str()
            .is_some_and(|line| line.contains("physical_plan=")))));
}

#[test]
fn explain_only_executes_mutations_when_analyze_is_requested() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();

    eng.sql("EXPLAIN INSERT INTO t (id) VALUES (1)", &[])
        .unwrap();
    eng.sql("EXPLAIN (ANALYZE false) INSERT INTO t (id) VALUES (2)", &[])
        .unwrap();
    assert!(eng.sql("SELECT id FROM t", &[]).unwrap().rows.is_empty());

    let analyzed = eng
        .sql("EXPLAIN ANALYZE INSERT INTO t (id) VALUES (1)", &[])
        .unwrap();
    let text = analyzed
        .rows
        .iter()
        .filter_map(|row| match row.get("plan") {
            Some(uqa_core::Value::Str(value)) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("affected_rows=1"), "{text}");
    assert_eq!(eng.sql("SELECT id FROM t", &[]).unwrap().rows.len(), 1);
}
