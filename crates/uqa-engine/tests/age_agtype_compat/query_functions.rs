//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Entity functions and aggregates
// ---------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)]
fn entity_functions_match_age() {
    let eng = engine_with_ground_truth_graph();
    let g = "gtruth";
    // label() works on vertices AND edges; labels() only on vertices;
    // type() only on edges.
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) RETURN label(n)",
        "\"Person\"",
    );
    assert_agtype(
        &eng,
        g,
        "MATCH ()-[e:KNOWS]->() RETURN label(e)",
        "\"KNOWS\"",
    );
    assert_agtype(
        &eng,
        g,
        "MATCH ()-[e:KNOWS]->() RETURN type(e)",
        "\"KNOWS\"",
    );
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) RETURN labels(n)",
        "[\"Person\"]",
    );
    assert_cypher_error(
        &eng,
        g,
        "MATCH ()-[e:KNOWS]->() RETURN labels(e)",
        "labels() argument must be a vertex",
    );
    assert_cypher_error(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) RETURN type(n)",
        "type() argument must be an edge or null",
    );
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) RETURN properties(n)",
        "{\"age\": 30, \"name\": \"Alice\"}",
    );
    // keys() uses JSONB ordering (length first, ties bytewise).
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) RETURN keys(n)",
        "[\"age\", \"name\"]",
    );
    assert_agtype(&eng, g, "RETURN keys({b: 1, aa: 2})", "[\"b\", \"aa\"]");
    assert_agtype(
        &eng,
        g,
        "MATCH ()-[e:KNOWS]->() RETURN startNode(e)",
        "{\"id\": 844424930131969, \"label\": \"Person\", \
         \"properties\": {\"age\": 30, \"name\": \"Alice\"}}::vertex",
    );
    assert_agtype(
        &eng,
        g,
        "MATCH ()-[e:KNOWS]->() RETURN endNode(e)",
        "{\"id\": 844424930131970, \"label\": \"Person\", \
         \"properties\": {\"age\": 25, \"name\": \"Bob\"}}::vertex",
    );
    // Property index access on a vertex.
    assert_agtype(
        &eng,
        g,
        "MATCH (a:Person {name: 'Alice'}) RETURN a['name']",
        "\"Alice\"",
    );
    // exists() on properties and patterns.
    assert_eq!(
        agtype_rows(
            &eng,
            g,
            "MATCH (n:Person) WHERE exists(n.age) RETURN n.name ORDER BY n.name"
        ),
        vec![Some("\"Alice\"".to_string()), Some("\"Bob\"".to_string())]
    );
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person {name: 'Alice'}) WHERE exists((n)-[:KNOWS]->()) RETURN n.name",
        "\"Alice\"",
    );
    // length() on paths; size() rejected on paths.
    assert_agtype(&eng, g, "MATCH p = ()-[:KNOWS]->() RETURN length(p)", "1");
    assert_cypher_error(
        &eng,
        g,
        "RETURN length('hello')",
        "length() argument must resolve to a path or null",
    );
    assert_cypher_error(
        &eng,
        g,
        "RETURN length([1,2])",
        "length() argument must resolve to a scalar",
    );
    // CASE expressions.
    assert_agtype(
        &eng,
        g,
        "RETURN CASE WHEN 1 = 1 THEN 'yes' ELSE 'no' END",
        "\"yes\"",
    );
    assert_agtype(
        &eng,
        g,
        "RETURN CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' END",
        "\"two\"",
    );
    assert_sql_null(&eng, g, "RETURN CASE 3 WHEN 1 THEN 'one' END");
}

#[test]
fn aggregates_match_age() {
    let eng = engine_with_ground_truth_graph();
    let g = "gtruth";
    assert_agtype(&eng, g, "MATCH (n:Person) RETURN count(*)", "2");
    assert_agtype(
        &eng,
        g,
        "MATCH (n:Person) RETURN collect(n.name)",
        "[\"Alice\", \"Bob\"]",
    );
    // sum of ints stays int; avg is float; min/max preserve type.
    let sql = "SELECT * FROM cypher('gtruth', $$
                   MATCH (n:Person)
                   RETURN sum(n.age), avg(n.age), min(n.age), max(n.age)
               $$) AS (a agtype, b agtype, c agtype, d agtype)";
    let result = eng.sql(sql, &[]).unwrap();
    let row = &result.rows[0];
    assert_eq!(row.get("a"), Some(&Value::Str("55".into())));
    assert_eq!(row.get("b"), Some(&Value::Str("27.5".into())));
    assert_eq!(row.get("c"), Some(&Value::Str("25".into())));
    assert_eq!(row.get("d"), Some(&Value::Str("30".into())));
    // Aggregates over zero rows: count 0, collect [], sum/avg null.
    let sql = "SELECT * FROM cypher('gtruth', $$
                   MATCH (n:NoSuch)
                   RETURN sum(n.age), count(n), collect(n.name)
               $$) AS (a agtype, b agtype, c agtype)";
    let result = eng.sql(sql, &[]).unwrap();
    let row = &result.rows[0];
    assert_eq!(row.get("a"), Some(&Value::Null));
    assert_eq!(row.get("b"), Some(&Value::Str("0".into())));
    assert_eq!(row.get("c"), Some(&Value::Str("[]".into())));
    // min/max work on strings; sum promotes on mixed numerics.
    assert_agtype(&eng, g, "UNWIND ['b','a','c'] AS x RETURN min(x)", "\"a\"");
    assert_agtype(&eng, g, "UNWIND ['b','a','c'] AS x RETURN max(x)", "\"c\"");
    assert_agtype(&eng, g, "UNWIND [1,2,3] AS x RETURN sum(x)", "6");
    assert_agtype(&eng, g, "UNWIND [1,2.5] AS x RETURN sum(x)", "3.5");
    assert_agtype(&eng, g, "UNWIND [1,2] AS x RETURN avg(x)", "1.5");
    assert_agtype(&eng, g, "UNWIND [1,1,2] AS x RETURN count(DISTINCT x)", "2");
    assert_agtype(
        &eng,
        g,
        "UNWIND [1,1,2] AS x RETURN collect(DISTINCT x)",
        "[1, 2]",
    );
    // Missing properties are skipped by count(prop).
    exec(
        &eng,
        "SELECT * FROM cypher('gtruth', $$ CREATE (:Person {name: 'Carol'}) $$) AS (v agtype)",
    );
    assert_agtype(&eng, g, "MATCH (n:Person) RETURN count(n.age)", "2");
    assert_agtype(&eng, g, "MATCH (n:Person) RETURN count(*)", "3");
}

// ---------------------------------------------------------------------
// SQL column type coercion at the cypher() boundary
// ---------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)]
fn cypher_output_coerces_to_declared_sql_types() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('coerce')");
    let one = |sql: &str| -> Value {
        let result = eng.sql(sql, &[]).unwrap();
        result.rows[0].get("v").cloned().unwrap()
    };
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 1 $$) AS (v int)"),
        Value::Int(1)
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 1 $$) AS (v bigint)"),
        Value::Int(1)
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 1 $$) AS (v float)"),
        Value::Float(1.0)
    );
    // float -> int rounds half to even (PostgreSQL cast).
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 1.9 $$) AS (v int)"),
        Value::Int(2)
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 0.5 $$) AS (v int)"),
        Value::Int(0)
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 2.5 $$) AS (v int)"),
        Value::Int(2)
    );
    // bool -> int works; int -> bool does not.
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN true $$) AS (v int)"),
        Value::Int(1)
    );
    let err = eng
        .sql(
            "SELECT * FROM cypher('coerce', $$ RETURN 1 $$) AS (v boolean)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot cast agtype integer to type boolean"),
        "{err}"
    );
    // Strings re-parse through agtype for numeric casts.
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN '42' $$) AS (v int)"),
        Value::Int(42)
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN '1.5' $$) AS (v float)"),
        Value::Float(1.5)
    );
    let err = eng
        .sql(
            "SELECT * FROM cypher('coerce', $$ RETURN 'abc' $$) AS (v int)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("invalid input syntax for type agtype"),
        "{err}"
    );
    // int4 range enforcement.
    let err = eng
        .sql(
            "SELECT * FROM cypher('coerce', $$ RETURN 9223372036854775807 $$) AS (v int)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("integer out of range"), "{err}");
    let err = eng
        .sql(
            "SELECT * FROM cypher('coerce', $$ RETURN 9223372036854775808.0 $$) AS (v bigint)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("integer out of range"), "{err}");
    // text: strings raw, non-entities agtype-rendered, entities refuse.
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 'abc' $$) AS (v text)"),
        Value::Str("abc".into())
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 1.5 $$) AS (v text)"),
        Value::Str("1.5".into())
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN [1,2] $$) AS (v text)"),
        Value::Str("[1, 2]".into())
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN {a:1} $$) AS (v text)"),
        Value::Str("{\"a\": 1}".into())
    );
    exec(
        &eng,
        "SELECT * FROM cypher('coerce', $$ CREATE (:Person {name: 'Alice'}) $$) AS (v agtype)",
    );
    let err = eng
        .sql(
            "SELECT * FROM cypher('coerce', $$ MATCH (n:Person) RETURN n $$) AS (v text)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("agtype_value_to_text: unsupported argument agtype 6"),
        "{err}"
    );
    // NULL passes through every declared type.
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN null $$) AS (v int)"),
        Value::Null
    );
    // Scalars in agtype columns render as agtype text.
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN 'x' $$) AS (v agtype)"),
        Value::Str("\"x\"".into())
    );
    assert_eq!(
        one("SELECT * FROM cypher('coerce', $$ RETURN count(*) $$) AS (v agtype)"),
        Value::Str("1".into())
    );
}
