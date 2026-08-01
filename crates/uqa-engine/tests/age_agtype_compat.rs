//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE 1.6.0 agtype compatibility matrix.
//!
//! Every expected string in this file was captured verbatim from a
//! live `PostgreSQL` 17.7 + AGE 1.6.0 container (`LOAD 'age'` +
//! `cypher(...)`), including graph ids: a fresh graph makes the AGE
//! `graphid` scheme (`label_id << 48 | sequence`, user labels from 3)
//! fully deterministic.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|err| panic!("SQL failed:\n{sql}\n{err:?}"));
}

/// Run one cypher RETURN through the SQL boundary with a single
/// `agtype` output column and hand back the rendered texts (None for
/// SQL NULL).
fn agtype_rows(engine: &Engine, graph: &str, query: &str) -> Vec<Option<String>> {
    let sql = format!("SELECT * FROM cypher('{graph}', $$ {query} $$) AS (v agtype)");
    let result = engine
        .sql(&sql, &[])
        .unwrap_or_else(|err| panic!("cypher failed:\n{query}\n{err:?}"));
    result
        .rows
        .iter()
        .map(|row| match row.get("v") {
            Some(Value::Str(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            other => panic!("agtype column must render as text, got {other:?}"),
        })
        .collect()
}

fn agtype_one(engine: &Engine, graph: &str, query: &str) -> Option<String> {
    let rows = agtype_rows(engine, graph, query);
    assert_eq!(rows.len(), 1, "expected one row from {query}");
    rows.into_iter().next().unwrap()
}

/// Assert `RETURN <expr>` renders exactly `expected` (agtype text).
fn assert_agtype(engine: &Engine, graph: &str, query: &str, expected: &str) {
    assert_eq!(
        agtype_one(engine, graph, query).as_deref(),
        Some(expected),
        "query: {query}"
    );
}

fn assert_sql_null(engine: &Engine, graph: &str, query: &str) {
    assert_eq!(agtype_one(engine, graph, query), None, "query: {query}");
}

fn assert_cypher_error(engine: &Engine, graph: &str, query: &str, needle: &str) {
    let sql = format!("SELECT * FROM cypher('{graph}', $$ {query} $$) AS (v agtype)");
    let err = match engine.sql(&sql, &[]) {
        Ok(result) => panic!("cypher unexpectedly succeeded:\n{query}\n{result:?}"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains(needle),
        "expected `{needle}` in error for {query}, got: {err}"
    );
}

fn engine_with_ground_truth_graph() -> Engine {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('gtruth')");
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$
             CREATE (a:Person {name: 'Alice', age: 30}),
                    (b:Person {name: 'Bob', age: 25})
         $$) AS (v agtype)",
    );
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$
             MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
             CREATE (a)-[e:KNOWS {since: 2020}]->(b)
         $$) AS (v agtype)",
    );
    eng
}

#[path = "age_agtype_compat/graph_contracts.rs"]
mod graph_contracts;

#[path = "age_agtype_compat/scalar_semantics.rs"]
mod scalar_semantics;

#[path = "age_agtype_compat/query_functions.rs"]
mod query_functions;

#[path = "age_agtype_compat/persistence.rs"]
mod persistence;
