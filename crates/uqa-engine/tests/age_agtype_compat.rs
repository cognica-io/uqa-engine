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

// ---------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------

#[test]
fn arithmetic_matches_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('arith')");
    let g = "arith";
    assert_agtype(&eng, g, "RETURN 1/3", "0");
    assert_agtype(&eng, g, "RETURN -7/3", "-2");
    assert_agtype(&eng, g, "RETURN 7%3", "1");
    assert_agtype(&eng, g, "RETURN -7%3", "-1");
    assert_agtype(&eng, g, "RETURN 7%-3", "1");
    assert_agtype(&eng, g, "RETURN 7.5%2", "1.5");
    assert_agtype(&eng, g, "RETURN -7.5%2", "-1.5");
    // `^` is ALWAYS float; unary minus binds tighter; left-assoc.
    assert_agtype(&eng, g, "RETURN 2^2", "4.0");
    assert_agtype(&eng, g, "RETURN 2^-1", "0.5");
    assert_agtype(&eng, g, "RETURN -2^2", "4.0");
    assert_agtype(&eng, g, "RETURN 2^3^2", "64.0");
    assert_cypher_error(&eng, g, "RETURN 1/0", "division by zero");
    assert_cypher_error(&eng, g, "RETURN 1.0/0.0", "division by zero");
    assert_cypher_error(&eng, g, "RETURN 0.0/0.0", "division by zero");
    // AGE quirks verified on 1.6.0: n % 0 returns n; float % 0 is NaN.
    assert_agtype(&eng, g, "RETURN 5%0", "5");
    assert_agtype(&eng, g, "RETURN 1.5%0.0", "NaN");
    // int64 arithmetic wraps.
    assert_agtype(
        &eng,
        g,
        "RETURN 9223372036854775807 + 1",
        "-9223372036854775808",
    );
    assert_sql_null(&eng, g, "RETURN sqrt(-1)");
    assert_sql_null(&eng, g, "RETURN log(0)");
    assert_sql_null(&eng, g, "RETURN log(-1)");
    assert_agtype(&eng, g, "RETURN sqrt(4)", "2.0");
    assert_agtype(&eng, g, "RETURN abs(-3)", "3");
    assert_agtype(&eng, g, "RETURN abs(-3.5)", "3.5");
    assert_agtype(&eng, g, "RETURN sign(-2)", "-1");
    assert_agtype(&eng, g, "RETURN sign(0)", "0");
    assert_agtype(&eng, g, "RETURN sign(2.5)", "1");
    assert_agtype(&eng, g, "RETURN ceil(0.1)", "1.0");
    assert_agtype(&eng, g, "RETURN ceil(2)", "2.0");
    assert_agtype(&eng, g, "RETURN floor(0.9)", "0.0");
    assert_agtype(&eng, g, "RETURN round(0.5)", "1.0");
    assert_agtype(&eng, g, "RETURN round(-0.5)", "-1.0");
    assert_agtype(&eng, g, "RETURN toFloat(1)/3", "0.3333333333333333");
    assert_agtype(&eng, g, "RETURN 2.0^63", "9.223372036854776e+18");
    assert_agtype(&eng, g, "RETURN 0.1 + 0.2", "0.30000000000000004");
    // Mixed int/float promotes to float.
    assert_agtype(&eng, g, "RETURN 3 + 4.0", "7.0");
    assert_agtype(&eng, g, "RETURN 5.0 / 2", "2.5");
    // Float rendering matrix.
    assert_agtype(&eng, g, "RETURN 100.0", "100.0");
    assert_agtype(&eng, g, "RETURN -0.0", "-0.0");
    assert_agtype(&eng, g, "RETURN 1e15", "1e+15");
    assert_agtype(&eng, g, "RETURN 1e14", "100000000000000.0");
    assert_agtype(&eng, g, "RETURN 0.0001", "0.0001");
    assert_agtype(&eng, g, "RETURN 0.00001", "1e-05");
    assert_agtype(&eng, g, "RETURN 1e100", "1e+100");
    assert_agtype(&eng, g, "RETURN 1e308 * 10", "Infinity");
    assert_agtype(&eng, g, "RETURN -1e308 * 10", "-Infinity");
}

// ---------------------------------------------------------------------
// ORDER BY type ordering
// ---------------------------------------------------------------------

#[test]
fn order_by_uses_agtype_type_order() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('ordering')");
    // Ascending: object < list < string < bool < number < null.
    let asc = agtype_rows(
        &eng,
        "ordering",
        "UNWIND [1,'a',null,2.5,true,[1],{x:1}] AS v RETURN v ORDER BY v",
    );
    assert_eq!(
        asc,
        vec![
            Some("{\"x\": 1}".to_string()),
            Some("[1]".to_string()),
            Some("\"a\"".to_string()),
            Some("true".to_string()),
            Some("1".to_string()),
            Some("2.5".to_string()),
            None,
        ]
    );
    // Descending is the exact reverse (null first).
    let desc = agtype_rows(
        &eng,
        "ordering",
        "UNWIND [1,'a',null,2.5,true,[1],{x:1}] AS v RETURN v ORDER BY v DESC",
    );
    assert_eq!(
        desc,
        vec![
            None,
            Some("2.5".to_string()),
            Some("1".to_string()),
            Some("true".to_string()),
            Some("\"a\"".to_string()),
            Some("[1]".to_string()),
            Some("{\"x\": 1}".to_string()),
        ]
    );
    // Comparison operators use the same total order.
    assert_agtype(&eng, "ordering", "RETURN 1 < 'a'", "false");
    assert_agtype(&eng, "ordering", "RETURN 'a' < 1", "true");
    assert_agtype(&eng, "ordering", "RETURN true < 1", "true");
    assert_agtype(&eng, "ordering", "RETURN [1] < 'a'", "true");
    assert_agtype(&eng, "ordering", "RETURN {a:1} < [1]", "true");
    assert_agtype(&eng, "ordering", "RETURN 'ab' < 'b'", "true");
    assert_agtype(&eng, "ordering", "RETURN [1,2] < [1,2,3]", "true");
    assert_agtype(&eng, "ordering", "RETURN 1 = 1.0", "true");
    assert_agtype(&eng, "ordering", "RETURN 'a' = 1", "false");
    // Comparisons chain left-associatively: (1 < 2) < 3.
    assert_agtype(&eng, "ordering", "RETURN 1 < 2 < 3", "true");
}

// ---------------------------------------------------------------------
// NULL semantics
// ---------------------------------------------------------------------

#[test]
fn null_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('nulls')");
    let g = "nulls";
    assert_sql_null(&eng, g, "RETURN null");
    assert_sql_null(&eng, g, "RETURN null = null");
    assert_sql_null(&eng, g, "RETURN null <> 1");
    assert_sql_null(&eng, g, "RETURN null + 1");
    assert_sql_null(&eng, g, "RETURN 1 < null");
    assert_sql_null(&eng, g, "RETURN -null");
    assert_agtype(&eng, g, "RETURN null IS NULL", "true");
    assert_agtype(&eng, g, "RETURN [null][0] IS NULL", "true");
    // Nested null renders as `null`.
    assert_agtype(&eng, g, "RETURN [null, 1]", "[null, 1]");
    // Three-valued logic (strict boolean operands).
    assert_sql_null(&eng, g, "RETURN true AND null");
    assert_agtype(&eng, g, "RETURN false AND null", "false");
    assert_agtype(&eng, g, "RETURN true OR null", "true");
    assert_sql_null(&eng, g, "RETURN false OR null");
    assert_sql_null(&eng, g, "RETURN NOT null");
    assert_sql_null(&eng, g, "RETURN null XOR true");
    assert_cypher_error(
        &eng,
        g,
        "RETURN 1 AND true",
        "cannot cast agtype integer to type boolean",
    );
    // WHERE requires a boolean (or null, which filters). The graph
    // needs a row for the per-row evaluation to hit the cast.
    exec(
        &eng,
        "SELECT * FROM cypher('nulls', $$ CREATE (:Thing) $$) AS (v agtype)",
    );
    assert_cypher_error(
        &eng,
        g,
        "MATCH (n) WHERE 1 RETURN n",
        "cannot cast agtype integer to type boolean",
    );
    assert_eq!(
        agtype_rows(&eng, g, "UNWIND [1] AS x WITH x WHERE null RETURN x").len(),
        0
    );
    assert_sql_null(&eng, g, "RETURN coalesce(null, null)");
    assert_agtype(&eng, g, "RETURN coalesce(null, 5)", "5");
    assert_sql_null(&eng, g, "RETURN size(null)");
    assert_sql_null(&eng, g, "RETURN toUpper(null)");
    assert_sql_null(&eng, g, "RETURN 'abc' STARTS WITH null");
    assert_sql_null(&eng, g, "RETURN id(null)");
    assert_sql_null(&eng, g, "RETURN type(null)");
    assert_sql_null(&eng, g, "RETURN startNode(null)");
}

// ---------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------

#[test]
fn list_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('lists')");
    let g = "lists";
    // Slices are end-exclusive; negative offsets wrap; bounds clamp.
    assert_agtype(&eng, g, "RETURN [1,2,3][0..2]", "[1, 2]");
    assert_agtype(&eng, g, "RETURN [1,2,3][-1]", "3");
    assert_agtype(&eng, g, "RETURN [1,2,3][-2..]", "[2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2,3][..2]", "[1, 2]");
    assert_agtype(&eng, g, "RETURN [1,2,3][0..10]", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2,3][2..0]", "[]");
    assert_agtype(&eng, g, "RETURN [1,2,3][-10..-1]", "[1, 2]");
    assert_sql_null(&eng, g, "RETURN [1,2,3][5]");
    // range() is end-INCLUSIVE and honors negative steps.
    assert_agtype(&eng, g, "RETURN range(0,10,3)", "[0, 3, 6, 9]");
    assert_agtype(&eng, g, "RETURN range(0,3)", "[0, 1, 2, 3]");
    assert_agtype(&eng, g, "RETURN range(3,0,-1)", "[3, 2, 1, 0]");
    assert_cypher_error(
        &eng,
        g,
        "RETURN range(1,5,0)",
        "range(): step cannot be zero",
    );
    // List comprehensions.
    assert_agtype(
        &eng,
        g,
        "RETURN [x IN [1,2,3] WHERE x > 1 | x * 10]",
        "[20, 30]",
    );
    assert_agtype(&eng, g, "RETURN [x IN [1,2,3] WHERE x > 1]", "[2, 3]");
    assert_agtype(&eng, g, "RETURN [x IN [1,2,3] | x * 10]", "[10, 20, 30]");
    // IN with null-aware membership.
    assert_agtype(&eng, g, "RETURN 1 IN [1,2]", "true");
    assert_agtype(&eng, g, "RETURN 3 IN [1,2]", "false");
    assert_agtype(&eng, g, "RETURN 1 IN [1, null]", "true");
    assert_sql_null(&eng, g, "RETURN 3 IN [1, null]");
    assert_sql_null(&eng, g, "RETURN null IN [1,2]");
    assert_sql_null(&eng, g, "RETURN 1 IN null");
    assert_cypher_error(&eng, g, "RETURN 1 IN 5", "object of IN must be a list");
    // Concatenation: list + list, list + scalar, scalar + list.
    assert_agtype(&eng, g, "RETURN [1,2] + [3]", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2] + 3", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN 2 + [1]", "[2, 1]");
    // Map merge.
    assert_agtype(&eng, g, "RETURN {a: 1} + {b: 2}", "{\"a\": 1, \"b\": 2}");
    // head/last/tail.
    assert_agtype(&eng, g, "RETURN head([1,2,3])", "1");
    assert_agtype(&eng, g, "RETURN last([1,2,3])", "3");
    assert_agtype(&eng, g, "RETURN tail([1,2,3])", "[2, 3]");
    assert_agtype(&eng, g, "RETURN reverse([1,2,3])", "[3, 2, 1]");
    // Map literals with nesting render in JSONB key order.
    assert_agtype(
        &eng,
        g,
        "RETURN {x: 1, y: 'a', nested: {z: [1,2]}}",
        "{\"x\": 1, \"y\": \"a\", \"nested\": {\"z\": [1, 2]}}",
    );
    assert_agtype(&eng, g, "RETURN {a: {b: 2}}.a.b", "2");
    assert_agtype(&eng, g, "RETURN {a: 1}['a']", "1");
    assert_sql_null(&eng, g, "RETURN {a: 1}.missing");
    assert_cypher_error(&eng, g, "RETURN 'hello'[0..2]", "slice must access a list");
    assert_cypher_error(
        &eng,
        g,
        "WITH 5 AS v RETURN v.x",
        "scalar object must be a vertex or edge",
    );
    // UNWIND over a bare scalar yields that scalar.
    assert_agtype(&eng, g, "UNWIND 5 AS x RETURN x", "5");
}

// ---------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------

#[test]
fn string_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('strings')");
    let g = "strings";
    assert_agtype(&eng, g, "RETURN toUpper('aB')", "\"AB\"");
    assert_agtype(&eng, g, "RETURN toLower('aB')", "\"ab\"");
    // substring is 0-based and character-oriented.
    assert_agtype(&eng, g, "RETURN substring('hello', 1, 3)", "\"ell\"");
    assert_agtype(&eng, g, "RETURN substring('hello', 1)", "\"ello\"");
    assert_agtype(&eng, g, "RETURN substring('hello', 10)", "\"\"");
    assert_cypher_error(
        &eng,
        g,
        "RETURN substring('hello', -1)",
        "substring() negative values are not supported for offset or length",
    );
    assert_agtype(
        &eng,
        g,
        "RETURN split('a,b,c', ',')",
        "[\"a\", \"b\", \"c\"]",
    );
    assert_agtype(&eng, g, "RETURN replace('aaa', 'a', 'b')", "\"bbb\"");
    assert_agtype(&eng, g, "RETURN trim('  x ')", "\"x\"");
    assert_agtype(&eng, g, "RETURN lTrim('  x ')", "\"x \"");
    assert_agtype(&eng, g, "RETURN rTrim('  x ')", "\"  x\"");
    assert_agtype(&eng, g, "RETURN left('hello', 2)", "\"he\"");
    assert_agtype(&eng, g, "RETURN right('hello', 2)", "\"lo\"");
    assert_cypher_error(
        &eng,
        g,
        "RETURN left('abc', -1)",
        "left() negative values are not supported for length",
    );
    assert_agtype(&eng, g, "RETURN reverse('abc')", "\"cba\"");
    // `=~` is an UNANCHORED regex search (PostgreSQL `~` semantics).
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'a.*'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'b'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' =~ '^b'", "false");
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'd'", "false");
    assert_agtype(&eng, g, "RETURN 'Abc' =~ '(?i)a.*'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' STARTS WITH 'a'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' ENDS WITH 'c'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' CONTAINS 'b'", "true");
    // Non-string operands compare false (not an error).
    assert_agtype(&eng, g, "RETURN 'abc' STARTS WITH 1", "false");
    // String concatenation coerces scalars (AGE renders bools empty).
    assert_agtype(&eng, g, "RETURN 'a' + 'b'", "\"ab\"");
    assert_agtype(&eng, g, "RETURN 'a' + 1", "\"a1\"");
    assert_agtype(&eng, g, "RETURN 1 + 'a'", "\"1a\"");
    assert_agtype(&eng, g, "RETURN 'a' + 1.5", "\"a1.5\"");
    assert_agtype(&eng, g, "RETURN 'a' + true", "\"a\"");
    // size() counts BYTES on strings, elements on lists.
    assert_agtype(&eng, g, "RETURN size('hello')", "5");
    assert_agtype(&eng, g, "RETURN size([1,2])", "2");
    assert_cypher_error(
        &eng,
        g,
        "RETURN size({a: 1})",
        "size() unsupported argument",
    );
    // Conversions.
    assert_agtype(&eng, g, "RETURN toInteger('42')", "42");
    assert_agtype(&eng, g, "RETURN toInteger('4.9')", "4");
    assert_agtype(&eng, g, "RETURN toInteger(4.9)", "4");
    assert_agtype(&eng, g, "RETURN toInteger(-4.9)", "-4");
    assert_sql_null(&eng, g, "RETURN toInteger('abc')");
    assert_agtype(&eng, g, "RETURN toFloat('1.5')", "1.5");
    assert_agtype(&eng, g, "RETURN toFloat(1)", "1.0");
    assert_sql_null(&eng, g, "RETURN toFloat('abc')");
    assert_agtype(&eng, g, "RETURN toBoolean('true')", "true");
    assert_agtype(&eng, g, "RETURN toBoolean('TRUE')", "true");
    assert_agtype(&eng, g, "RETURN toString(1)", "\"1\"");
    assert_agtype(&eng, g, "RETURN toString(1.5)", "\"1.5\"");
    // toString on floats uses raw float8out (no `.0` suffix).
    assert_agtype(&eng, g, "RETURN toString(1.0)", "\"1\"");
    assert_agtype(&eng, g, "RETURN toString(true)", "\"true\"");
    // Wrong-type arguments raise AGE's ordinal-tagged errors.
    assert_cypher_error(
        &eng,
        g,
        "RETURN toUpper(5)",
        "toUpper() unsupported argument agtype 3",
    );
    assert_cypher_error(
        &eng,
        g,
        "RETURN abs('x')",
        "abs() unsupported argument agtype 1",
    );
    // JSON escaping in rendered strings.
    assert_agtype(&eng, g, "RETURN 'a\"b\\n'", "\"a\\\"b\\n\"");
}

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

// ---------------------------------------------------------------------
// Label registry persistence
// ---------------------------------------------------------------------

#[test]
fn graphid_allocation_survives_engine_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("age_graphid.db");
    {
        let eng = Engine::open(&path).unwrap();
        exec(&eng, "SELECT create_graph('persist')");
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:Person {name: 'Alice'}), (:City {name: 'Seoul'})
             $$) AS (v agtype)",
        );
        // Delete the City so its label survives only via metadata.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 MATCH (c:City) DETACH DELETE c
             $$) AS (v agtype)",
        );
    }
    {
        let eng = Engine::open(&path).unwrap();
        // Person keeps label id 3; the next Person is sequence 2.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:Person {name: 'Bob'})
             $$) AS (v agtype)",
        );
        assert_eq!(
            agtype_rows(
                &eng,
                "persist",
                "MATCH (n:Person) RETURN id(n) ORDER BY id(n)"
            ),
            vec![
                Some("844424930131969".to_string()),
                Some("844424930131970".to_string()),
            ]
        );
        // City's label id (4) survives deletion of all its vertices,
        // so a NEW label continues at 5 and a recreated City vertex
        // resumes its sequence at 2.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:City {name: 'Busan'}), (:Country {name: 'KR'})
             $$) AS (v agtype)",
        );
        assert_agtype(
            &eng,
            "persist",
            "MATCH (c:City) RETURN id(c)",
            &((4_u64 << 48) | 2).to_string(),
        );
        assert_agtype(
            &eng,
            "persist",
            "MATCH (c:Country) RETURN id(c)",
            &((5_u64 << 48) | 1).to_string(),
        );
    }
}
