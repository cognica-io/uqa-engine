//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher round-trip stress test: build a 100-vertex graph through
//! the writer with `CREATE`, then exercise the read executor with
//! `MATCH` / `WHERE` / `RETURN` and confirm the projections come back
//! in the right order.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_graph::{
    cypher::{parse_cypher, CypherExecutor, CypherWriter},
    GraphStore, MemoryGraphStore,
};

const N: i64 = 100;

fn build_store() -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let mut writer = CypherWriter::new(&mut store, "g");
    for i in 0..N {
        let create = format!(
            "CREATE (n:Person {{ pid: {i}, name: 'p{i}', score: {} }})",
            i * 3 % 17,
        );
        let q = parse_cypher(&create).expect("parse create");
        writer.execute(&q).expect("execute create");
    }
    drop(writer);
    store
}

#[test]
fn cypher_round_trip_filters_and_orders() {
    let store = build_store();
    let exec = CypherExecutor::new(&store, "g");
    let q = parse_cypher(
        "MATCH (n:Person) WHERE n.score >= 10 RETURN n.pid AS pid, n.score AS score ORDER BY n.pid",
    )
    .expect("parse select");
    let (cols, rows) = exec.execute(&q).expect("execute select");
    assert_eq!(cols, vec!["pid".to_string(), "score".to_string()]);
    let pairs: Vec<(i64, i64)> = rows
        .iter()
        .filter_map(|row| match (row.get("pid"), row.get("score")) {
            (Some(Value::Int(p)), Some(Value::Int(s))) => Some((*p, *s)),
            _ => None,
        })
        .collect();
    let expected: Vec<(i64, i64)> = (0..N)
        .filter_map(|i| {
            let s = i * 3 % 17;
            (s >= 10).then_some((i, s))
        })
        .collect();
    assert_eq!(pairs, expected);
}

#[test]
fn cypher_round_trip_count_aggregate() {
    let store = build_store();
    let exec = CypherExecutor::new(&store, "g");
    let q = parse_cypher("MATCH (n:Person) RETURN count(*) AS n").expect("parse count");
    let (_, rows) = exec.execute(&q).expect("execute count");
    let n = match rows[0].get("n") {
        Some(Value::Int(n)) => *n,
        other => panic!("expected count int, got {other:?}"),
    };
    assert_eq!(n, N);
}

#[test]
fn cypher_round_trip_distinct() {
    let store = build_store();
    let exec = CypherExecutor::new(&store, "g");
    let q = parse_cypher("MATCH (n:Person) RETURN DISTINCT n.score AS score ORDER BY score")
        .expect("parse distinct");
    let (_, rows) = exec.execute(&q).expect("execute distinct");
    let scores: Vec<i64> = rows
        .iter()
        .filter_map(|r| match r.get("score") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    let mut expected: Vec<i64> = (0..N).map(|i| i * 3 % 17).collect();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(scores, expected);
}

#[test]
fn cypher_round_trip_carries_extra_fields() {
    // Spot-check: every created vertex made it into the store and the
    // properties match what we asked CREATE to write.
    let store = build_store();
    let exec = CypherExecutor::new(&store, "g");
    let q =
        parse_cypher("MATCH (n:Person {pid: 42}) RETURN n.name AS name").expect("parse pid filter");
    let (_, rows) = exec.execute(&q).expect("execute");
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["p42".to_string()]);
    // Sanity: there are exactly N vertices in the graph after the writer
    // ran.
    let count = store.vertex_ids_in_graph("g").unwrap().len();
    assert_eq!(count, N as usize);
    let _ = BTreeMap::<i64, i64>::new(); // ensure unused-import-free
}
