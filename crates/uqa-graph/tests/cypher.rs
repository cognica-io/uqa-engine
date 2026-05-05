//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end Cypher executor tests on `MemoryGraphStore`.

use std::collections::BTreeMap;

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{
    cypher::{parse_cypher, CypherExecutor},
    GraphStore, MemoryGraphStore,
};

fn corpus() -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g");
    let mk = |id: u64, label: &str, props: &[(&str, Value)]| {
        let mut v = Vertex::new(id, label);
        for (k, val) in props {
            v.properties.insert((*k).into(), val.clone());
        }
        v
    };
    g.add_vertex(
        mk(
            1,
            "Person",
            &[
                ("name", Value::Str("alice".into())),
                ("age", Value::Int(30)),
            ],
        ),
        "g",
    );
    g.add_vertex(
        mk(
            2,
            "Person",
            &[("name", Value::Str("bob".into())), ("age", Value::Int(40))],
        ),
        "g",
    );
    g.add_vertex(
        mk(
            3,
            "Person",
            &[
                ("name", Value::Str("carol".into())),
                ("age", Value::Int(25)),
            ],
        ),
        "g",
    );
    g.add_vertex(mk(10, "City", &[("name", Value::Str("sf".into()))]), "g");
    g.add_vertex(mk(11, "City", &[("name", Value::Str("ny".into()))]), "g");
    g.add_edge(Edge::new(100, 1, 2, "KNOWS"), "g");
    g.add_edge(Edge::new(101, 2, 3, "KNOWS"), "g");
    g.add_edge(Edge::new(110, 1, 10, "LIVES_IN"), "g");
    g.add_edge(Edge::new(111, 2, 10, "LIVES_IN"), "g");
    g.add_edge(Edge::new(112, 3, 11, "LIVES_IN"), "g");
    g
}

fn run(src: &str) -> (Vec<String>, Vec<BTreeMap<String, Value>>) {
    let g = corpus();
    let exec = CypherExecutor::new(&g, "g");
    let query = parse_cypher(src).unwrap();
    exec.execute(&query).unwrap()
}

#[test]
fn return_node_property() {
    let (cols, rows) = run("MATCH (n:Person) RETURN n.name");
    assert_eq!(cols, vec!["n.name".to_string()]);
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("n.name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"alice".into()));
}

#[test]
fn where_filters_results() {
    let (_, rows) = run("MATCH (n:Person) WHERE n.age > 28 RETURN n.name");
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("n.name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"alice".into()));
    assert!(names.contains(&"bob".into()));
}

#[test]
fn match_relationship_fixed_one_hop() {
    let (_, rows) = run("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name AS a, b.name AS b");
    let pairs: Vec<(String, String)> = rows
        .iter()
        .filter_map(|r| match (r.get("a"), r.get("b")) {
            (Some(Value::Str(a)), Some(Value::Str(b))) => Some((a.clone(), b.clone())),
            _ => None,
        })
        .collect();
    assert!(pairs.contains(&("alice".into(), "bob".into())));
    assert!(pairs.contains(&("bob".into(), "carol".into())));
}

#[test]
fn variable_length_path() {
    let (_, rows) =
        run("MATCH (a:Person {name: 'alice'})-[:KNOWS*1..2]->(b:Person) RETURN b.name AS name");
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    // alice -> bob (1 hop), alice -> bob -> carol (2 hops).
    assert!(names.contains(&"bob".into()));
    assert!(names.contains(&"carol".into()));
}

#[test]
fn aggregate_count_groups_by_label() {
    let (_, rows) = run("MATCH (p:Person)-[:LIVES_IN]->(c:City) \
         RETURN c.name AS city, count(*) AS n \
         ORDER BY city");
    let lookup: BTreeMap<String, i64> = rows
        .iter()
        .filter_map(|r| match (r.get("city"), r.get("n")) {
            (Some(Value::Str(c)), Some(Value::Int(n))) => Some((c.clone(), *n)),
            _ => None,
        })
        .collect();
    assert_eq!(lookup.get("sf"), Some(&2));
    assert_eq!(lookup.get("ny"), Some(&1));
}

#[test]
fn order_by_skip_limit() {
    let (_, rows) =
        run("MATCH (n:Person) RETURN n.name AS name ORDER BY n.age DESC SKIP 1 LIMIT 1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("alice".into())));
}

#[test]
fn distinct_dedupes_results() {
    let (_, rows) = run("MATCH (p:Person)-[:LIVES_IN]->(c:City) RETURN DISTINCT c.name AS city");
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("city") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len());
}

#[test]
fn with_pipeline_filters() {
    let (_, rows) = run("MATCH (n:Person) \
         WITH n.name AS name, n.age AS age WHERE age > 28 \
         RETURN name ORDER BY age DESC");
    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["bob".to_string(), "alice".to_string()]);
}

#[test]
fn optional_match_pads_unmatched() {
    let (_, rows) = run("MATCH (p:Person {name: 'alice'}) \
         OPTIONAL MATCH (p)-[:DOES_NOT_EXIST]->(x) \
         RETURN p.name AS name");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&Value::Str("alice".into())));
}

#[test]
fn function_calls_id_and_labels() {
    let (_, rows) =
        run("MATCH (n:Person) WHERE n.name = 'alice' RETURN id(n) AS i, labels(n) AS l");
    assert_eq!(rows[0].get("i"), Some(&Value::Int(1)));
    if let Some(Value::List(items)) = rows[0].get("l") {
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], Value::Str("Person".into()));
    } else {
        panic!("expected labels list");
    }
}
