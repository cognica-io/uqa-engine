//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Entity rendering (ground truth strings, ids included)
// ---------------------------------------------------------------------

#[test]
fn vertex_edge_and_path_render_in_age_format() {
    let eng = engine_with_ground_truth_graph();
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (a:Person {name: 'Alice'}) RETURN a",
        "{\"id\": 844424930131969, \"label\": \"Person\", \
         \"properties\": {\"age\": 30, \"name\": \"Alice\"}}::vertex",
    );
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH ()-[e:KNOWS]->() RETURN e",
        "{\"id\": 1125899906842625, \"label\": \"KNOWS\", \
         \"end_id\": 844424930131970, \"start_id\": 844424930131969, \
         \"properties\": {\"since\": 2020}}::edge",
    );
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH p = (a:Person {name: 'Alice'})-[e:KNOWS]->(b) RETURN p",
        "[{\"id\": 844424930131969, \"label\": \"Person\", \
         \"properties\": {\"age\": 30, \"name\": \"Alice\"}}::vertex, \
         {\"id\": 1125899906842625, \"label\": \"KNOWS\", \
         \"end_id\": 844424930131970, \"start_id\": 844424930131969, \
         \"properties\": {\"since\": 2020}}::edge, \
         {\"id\": 844424930131970, \"label\": \"Person\", \
         \"properties\": {\"age\": 25, \"name\": \"Bob\"}}::vertex]::path",
    );
    // Entities keep their ::vertex suffix nested in lists and maps.
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (a:Person {name: 'Alice'}) RETURN {v: a, k: 1}",
        "{\"k\": 1, \"v\": {\"id\": 844424930131969, \"label\": \"Person\", \
         \"properties\": {\"age\": 30, \"name\": \"Alice\"}}::vertex}",
    );
    // Variable-length relationships bind a plain LIST of edges.
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (a:Person {name: 'Alice'})-[e:KNOWS*1..2]->(b) RETURN e",
        "[{\"id\": 1125899906842625, \"label\": \"KNOWS\", \
         \"end_id\": 844424930131970, \"start_id\": 844424930131969, \
         \"properties\": {\"since\": 2020}}::edge]",
    );
}

#[test]
fn graphid_scheme_matches_age_label_allocation() {
    let eng = engine_with_ground_truth_graph();
    // First Person vertex: 3 << 48 | 1.
    let ids = agtype_rows(
        &eng,
        "gtruth",
        "MATCH (n:Person) RETURN id(n) ORDER BY id(n)",
    );
    assert_eq!(
        ids,
        vec![
            Some("844424930131969".to_string()),
            Some("844424930131970".to_string()),
        ]
    );
    // First KNOWS edge: 4 << 48 | 1 (labels share one counter).
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH ()-[e:KNOWS]->() RETURN id(e)",
        "1125899906842625",
    );
    // A later vertex label continues the shared counter: City -> 5.
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$ CREATE (c:City {name: 'Seoul'}) $$) AS (v agtype)",
    );
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (c:City) RETURN c",
        "{\"id\": 1407374883553281, \"label\": \"City\", \
         \"properties\": {\"name\": \"Seoul\"}}::vertex",
    );
    // Third Person -> 3 << 48 | 3.
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$ CREATE (:Person {name: 'Carol'}) $$) AS (v agtype)",
    );
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (n:Person {name: 'Carol'}) RETURN id(n)",
        "844424930131971",
    );
    // Unlabeled vertices use the reserved label id 1.
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$ CREATE (n {tag: 'anon'}) $$) AS (v agtype)",
    );
    assert_agtype(
        &eng,
        "gtruth",
        "MATCH (n {tag: 'anon'}) RETURN n",
        "{\"id\": 281474976710657, \"label\": \"\", \
         \"properties\": {\"tag\": \"anon\"}}::vertex",
    );
}

// ---------------------------------------------------------------------
// create_graph / drop_graph validation
// ---------------------------------------------------------------------

#[test]
fn create_graph_validates_names_like_age() {
    let eng = Engine::new();
    for invalid in ["ab", "a1", "1ab"] {
        let err = eng
            .sql(&format!("SELECT create_graph('{invalid}')"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("graph name is invalid"), "{invalid}: {err}");
    }
    for valid in ["abc", "ab1", "_ab", "AB2"] {
        exec(&eng, &format!("SELECT create_graph('{valid}')"));
    }
    let err = eng
        .sql("SELECT create_graph(NULL)", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("graph name can not be NULL"), "{err}");

    // cypher() on a missing graph matches AGE's message.
    let err = eng
        .sql(
            "SELECT * FROM cypher('no_such_graph', $$ RETURN 1 $$) AS (v agtype)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("graph \"no_such_graph\" does not exist"),
        "{err}"
    );
}

#[test]
fn cypher_column_count_mismatch_matches_age_error() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('mismatch')");
    for query in ["RETURN 1, 2", "RETURN 1"] {
        let sql = if query == "RETURN 1" {
            format!("SELECT * FROM cypher('mismatch', $$ {query} $$) AS (a agtype, b agtype)")
        } else {
            format!("SELECT * FROM cypher('mismatch', $$ {query} $$) AS (a agtype)")
        };
        let err = eng.sql(&sql, &[]).unwrap_err().to_string();
        assert!(
            err.contains("return row and column definition list do not match"),
            "{query}: {err}"
        );
    }
}
