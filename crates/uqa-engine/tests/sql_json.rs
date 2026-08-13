//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL JSON coverage.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn json(v: &str) -> Value {
    Value::Json(v.to_string())
}

fn jsonb(v: &str) -> Value {
    Value::JsonB(v.to_string())
}

fn engine_with_json() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, data JSON, label TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO docs (id, data, label) VALUES
         (1, '{\"name\": \"Alice\", \"age\": 30, \"tags\": [\"a\", \"b\"]}', 'first')",
    );
    exec(
        &engine,
        "INSERT INTO docs (id, data, label) VALUES
         (2, '{\"name\": \"Bob\", \"age\": 25, \"tags\": [\"c\"]}', 'second')",
    );
    exec(
        &engine,
        "INSERT INTO docs (id, data, label) VALUES
         (3, '{\"name\": \"Carol\", \"nested\": {\"x\": 10}}', 'third')",
    );
    engine
}

fn engine_with_table() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (1, 10, 'alpha')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (2, 20, 'bravo')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (3, 30, 'charlie')",
    );
    engine
}

#[test]
fn create_table_with_json() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, data JSON)",
    );
    assert!(result.rows.is_empty());
    let selected = exec(&engine, "SELECT * FROM t");
    assert!(selected.columns.contains(&"data".to_string()));
}

#[test]
fn create_table_with_jsonb() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, data JSONB)",
    );
    let selected = exec(&engine, "SELECT * FROM t");
    assert!(selected.columns.contains(&"data".to_string()));
}

#[test]
fn json_and_jsonb_catalog_types_are_distinct() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE typed_json (id INTEGER PRIMARY KEY, raw JSON, bin JSONB)",
    );
    let result = exec(
        &engine,
        "SELECT column_name, data_type, udt_name
         FROM information_schema.columns
         WHERE table_name = 'typed_json'
         ORDER BY column_name",
    );
    let row_for = |name: &str| {
        result
            .rows
            .iter()
            .find(|row| row["column_name"] == s(name))
            .unwrap()
    };
    assert_eq!(row_for("raw")["data_type"], s("json"));
    assert_eq!(row_for("raw")["udt_name"], s("json"));
    assert_eq!(row_for("bin")["data_type"], s("jsonb"));
    assert_eq!(row_for("bin")["udt_name"], s("jsonb"));
}

#[test]
fn insert_json_string_preserves_json_text() {
    let engine = engine_with_json();
    let result = exec(&engine, "SELECT data FROM docs WHERE id = 1");
    assert_eq!(
        result.rows[0]["data"],
        json("{\"name\": \"Alice\", \"age\": 30, \"tags\": [\"a\", \"b\"]}")
    );
}

#[test]
fn insert_json_array_preserves_json_type() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, items JSON)",
    );
    exec(&engine, "INSERT INTO t (id, items) VALUES (1, '[1, 2, 3]')");
    let result = exec(&engine, "SELECT items FROM t WHERE id = 1");
    assert_eq!(result.rows[0]["items"], json("[1, 2, 3]"));
}

#[test]
fn json_preserves_input_text_while_jsonb_canonicalizes() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"b\":2,\"a\":1}'::json AS raw,
                '{\"b\":2,\"a\":1}'::jsonb AS bin",
    );
    assert_eq!(result.rows[0]["raw"], json("{\"b\":2,\"a\":1}"));
    assert_eq!(result.rows[0]["bin"], jsonb("{\"a\": 1, \"b\": 2}"));
}

#[test]
fn arrow_text_key() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->'name' AS name FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["name"], json("\"Alice\""));
}

#[test]
fn double_arrow_text_key() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->>'name' AS name FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["name"], s("Alice"));
}

#[test]
fn arrow_integer_key() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->'tags'->0 AS first_tag FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["first_tag"], json("\"a\""));
}

#[test]
fn arrow_nested_object() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->'nested'->'x' AS x FROM docs WHERE id = 3",
    );
    assert_eq!(result.rows[0]["x"], json("10"));
}

#[test]
fn double_arrow_returns_text_for_nested() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->>'tags' AS tags FROM docs WHERE id = 1",
    );
    let Value::Str(tags) = &result.rows[0]["tags"] else {
        panic!("expected text");
    };
    assert!(tags.contains("\"a\""));
}

#[test]
fn jsonb_double_arrow_uses_jsonb_output_text() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"a\": {\"b\": 2}}'::jsonb ->> 'a' AS value",
    );
    assert_eq!(result.rows[0]["value"], s("{\"b\": 2}"));
}

#[test]
fn arrow_missing_key_returns_null() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT data->'nonexistent' AS v FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Null);
}

#[test]
fn jsonb_negative_index_extracts_from_end() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT '[\"a\", \"b\", \"c\"]'::jsonb -> -1 AS v");
    assert_eq!(result.rows[0]["v"], jsonb("\"c\""));
}

#[test]
fn jsonb_key_exists_checks_string_array_elements() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '[\"a\", \"b\", \"c\"]'::jsonb ? 'b' AS has_b,
                '[\"a\", \"b\", \"c\"]'::jsonb ?| ARRAY['x', 'c'] AS has_any,
                '[\"a\", \"b\", \"c\"]'::jsonb ?& ARRAY['a', 'b'] AS has_all",
    );
    assert_eq!(result.rows[0]["has_b"], Value::Bool(true));
    assert_eq!(result.rows[0]["has_any"], Value::Bool(true));
    assert_eq!(result.rows[0]["has_all"], Value::Bool(true));
}

#[test]
fn jsonb_delete_key_and_path() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"a\":1,\"b\":{\"c\":2,\"d\":3}}'::jsonb - 'a' AS removed_key,
                '{\"a\":1,\"b\":{\"c\":2,\"d\":3}}'::jsonb #- '{b,c}' AS removed_path",
    );
    assert_eq!(
        result.rows[0]["removed_key"],
        jsonb("{\"b\": {\"c\": 2, \"d\": 3}}")
    );
    assert_eq!(
        result.rows[0]["removed_path"],
        jsonb("{\"a\": 1, \"b\": {\"d\": 3}}")
    );
}

#[test]
fn jsonb_concat_objects() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"a\":1}'::jsonb || '{\"b\":2}'::jsonb AS v",
    );
    assert_eq!(result.rows[0]["v"], jsonb("{\"a\": 1, \"b\": 2}"));
}

#[test]
fn jsonb_set_respects_create_missing_false() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT jsonb_set('{\"a\":1}'::jsonb, ARRAY['b'], '2'::jsonb, false) AS v",
    );
    assert_eq!(result.rows[0]["v"], jsonb("{\"a\": 1}"));
}

#[test]
fn jsonb_insert_array_before_and_after() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT jsonb_insert('{\"a\":[0,1,2]}'::jsonb, ARRAY['a','1'], '9'::jsonb) AS before,
                jsonb_insert('{\"a\":[0,1,2]}'::jsonb, ARRAY['a','1'], '9'::jsonb, true) AS after",
    );
    assert_eq!(result.rows[0]["before"], jsonb("{\"a\": [0, 9, 1, 2]}"));
    assert_eq!(result.rows[0]["after"], jsonb("{\"a\": [0, 1, 9, 2]}"));
}

#[test]
fn jsonb_insert_object_only_when_key_is_missing() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT jsonb_insert('{\"a\":1}'::jsonb, ARRAY['b'], '2'::jsonb) AS inserted,
                jsonb_insert('{\"a\":1}'::jsonb, ARRAY['a'], '2'::jsonb) AS unchanged",
    );
    assert_eq!(result.rows[0]["inserted"], jsonb("{\"a\": 1, \"b\": 2}"));
    assert_eq!(result.rows[0]["unchanged"], jsonb("{\"a\": 1}"));
}

#[test]
fn jsonb_pretty_returns_indented_text() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT jsonb_pretty('{\"a\":[1,2]}'::jsonb) AS v");
    let Value::Str(text) = &result.rows[0]["v"] else {
        panic!("expected text");
    };
    assert!(text.contains('\n'));
    assert!(text.contains("\"a\""));
}

#[test]
fn jsonb_path_exists_function_and_at_question_operator() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"a\":[1,2,3]}'::jsonb @? '$.a[*] ? (@ > 2)' AS op,
                jsonb_path_exists('{\"a\":[1,2,3]}'::jsonb, '$.a[*] ? (@ == 2)') AS fn",
    );
    assert_eq!(result.rows[0]["op"], Value::Bool(true));
    assert_eq!(result.rows[0]["fn"], Value::Bool(true));
}

#[test]
fn jsonb_path_match_function_and_at_at_operator() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT '{\"a\":1}'::jsonb @@ '$.a == 1' AS op,
                jsonb_path_match('{\"a\":1}'::jsonb, '$.a == 2') AS fn",
    );
    assert_eq!(result.rows[0]["op"], Value::Bool(true));
    assert_eq!(result.rows[0]["fn"], Value::Bool(false));
}

#[test]
fn jsonpath_operators_filter_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE jpath (id INTEGER PRIMARY KEY, data JSONB)",
    );
    exec(
        &engine,
        "INSERT INTO jpath (id, data) VALUES
         (1, '{\"a\":1,\"tags\":[\"a\"]}'::jsonb),
         (2, '{\"a\":2,\"tags\":[\"a\",\"b\"]}'::jsonb)",
    );

    let matched = exec(
        &engine,
        "SELECT id FROM jpath WHERE data @@ '$.a == 2' ORDER BY id",
    );
    assert_eq!(matched.rows.len(), 1);
    assert_eq!(matched.rows[0]["id"], Value::Int(2));

    let exists = exec(
        &engine,
        "SELECT id FROM jpath WHERE data @? '$.tags[*] ? (@ == \"b\")' ORDER BY id",
    );
    assert_eq!(exists.rows.len(), 1);
    assert_eq!(exists.rows[0]["id"], Value::Int(2));
}

#[test]
fn json_in_where() {
    let engine = engine_with_json();
    let result = exec(&engine, "SELECT id FROM docs WHERE data->>'name' = 'Bob'");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(2));
}

#[test]
fn json_build_object_returns_json() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO t (id) VALUES (1)");
    let result = exec(
        &engine,
        "SELECT json_build_object('a', 1, 'b', 2) AS obj FROM t",
    );
    assert_eq!(result.rows[0]["obj"], json("{\"a\" : 1, \"b\" : 2}"));
}

#[test]
fn json_build_array_returns_json() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO t (id) VALUES (1)");
    let result = exec(&engine, "SELECT json_build_array(1, 2, 3) AS arr FROM t");
    assert_eq!(result.rows[0]["arr"], json("[1, 2, 3]"));
}

#[test]
fn json_build_array_mixed_types_preserves_types() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT json_build_array(1, 2, 3, 'four') AS arr");
    assert_eq!(result.rows[0]["arr"], json("[1, 2, 3, \"four\"]"));
}

#[test]
fn json_build_array_mixed_int_float_str_bool_preserves_types() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT json_build_array(1, 2.5, 'hello', true) AS arr",
    );
    assert_eq!(result.rows[0]["arr"], json("[1, 2.5, \"hello\", true]"));
}

#[test]
fn json_build_array_empty() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT json_build_array() AS arr");
    assert_eq!(result.rows[0]["arr"], json("[]"));
}

#[test]
fn json_typeof_object() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_typeof(data) AS t FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["t"], s("object"));
}

#[test]
fn json_typeof_array() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_typeof(data->'tags') AS t FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["t"], s("array"));
}

#[test]
fn json_array_length() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_array_length(data->'tags') AS n FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["n"], Value::Int(2));
}

#[test]
fn json_array_length_single() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_array_length(data->'tags') AS n FROM docs WHERE id = 2",
    );
    assert_eq!(result.rows[0]["n"], Value::Int(1));
}

#[test]
fn json_extract_path() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_extract_path(data, 'nested', 'x') AS v FROM docs WHERE id = 3",
    );
    assert_eq!(result.rows[0]["v"], json("10"));
}

#[test]
fn json_extract_path_text() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_extract_path_text(data, 'name') AS v FROM docs WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], s("Alice"));
}

#[test]
fn cast_to_json() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY, raw TEXT)");
    exec(&engine, "INSERT INTO t (id, raw) VALUES (1, '{\"x\": 42}')");
    let result = exec(
        &engine,
        "SELECT CAST(raw AS json)->'x' AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], json("42"));
}

#[test]
fn json_object_agg_basic() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT json_object_agg(name, val) AS v FROM t");
    assert_eq!(
        result.rows[0]["v"],
        json("{ \"alpha\" : 10, \"bravo\" : 20, \"charlie\" : 30 }")
    );
}

#[test]
fn jsonb_object_agg_variant() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT jsonb_object_agg(name, val) AS v FROM t");
    assert_eq!(
        result.rows[0]["v"],
        jsonb("{\"alpha\": 10, \"bravo\": 20, \"charlie\": 30}")
    );
}

#[test]
fn json_agg_orders_values() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT json_agg(data->>'name' ORDER BY id) AS v FROM docs",
    );
    assert_eq!(result.rows[0]["v"], json("[\"Alice\", \"Bob\", \"Carol\"]"));
}

#[test]
fn json_agg_includes_null_inputs() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER, v TEXT)");
    exec(
        &engine,
        "INSERT INTO t (id, v) VALUES (1, 'alpha'), (2, NULL), (3, 'charlie')",
    );
    let result = exec(&engine, "SELECT json_agg(v ORDER BY id) AS v FROM t");
    assert_eq!(result.rows[0]["v"], json("[\"alpha\", null, \"charlie\"]"));
}

#[test]
fn jsonb_agg_collects_json_values() {
    let engine = engine_with_json();
    let result = exec(
        &engine,
        "SELECT jsonb_agg(data->'tags' ORDER BY id) AS v FROM docs",
    );
    assert_eq!(
        result.rows[0]["v"],
        jsonb("[[\"a\", \"b\"], [\"c\"], null]")
    );
}

#[test]
fn hash_gt_path_operator() {
    let engine = engine_with_table();
    exec(
        &engine,
        "CREATE TABLE jdoc (id SERIAL PRIMARY KEY, data JSONB)",
    );
    exec(
        &engine,
        "INSERT INTO jdoc (data) VALUES ('{\"a\": {\"b\": 42}}'::jsonb)",
    );
    let result = exec(
        &engine,
        "SELECT data #> '{a,b}' AS v FROM jdoc WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], jsonb("42"));
}

#[test]
fn hash_gt_gt_path_operator() {
    let engine = engine_with_table();
    exec(
        &engine,
        "CREATE TABLE jd2 (id SERIAL PRIMARY KEY, data JSONB)",
    );
    exec(
        &engine,
        "INSERT INTO jd2 (data) VALUES ('{\"a\": {\"b\": 42}}'::jsonb)",
    );
    let result = exec(
        &engine,
        "SELECT data #>> '{a,b}' AS v FROM jd2 WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], s("42"));
}

#[test]
fn json_contains_operator() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2}'::jsonb @> '{\"a\": 1}'::jsonb AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn json_not_contains_operator() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1}'::jsonb @> '{\"a\": 2}'::jsonb AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(false));
}

#[test]
fn json_contained_by_operator() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1}'::jsonb <@ '{\"a\": 1, \"b\": 2}'::jsonb AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn jsonb_set_basic() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT jsonb_set('{\"a\": 1}'::jsonb, '{b}', '2'::jsonb) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], jsonb("{\"a\": 1, \"b\": 2}"));
}

#[test]
fn jsonb_set_replace() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT jsonb_set('{\"a\": 1}'::jsonb, '{a}', '99'::jsonb) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], jsonb("{\"a\": 99}"));
}

#[test]
fn json_object_keys_basic() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT json_object_keys('{\"a\": 1, \"b\": 2}'::json) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["v"], s("a"));
    assert_eq!(result.rows[1]["v"], s("b"));
}

#[test]
fn json_has_key_present() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2, \"c\": 3}'::jsonb ? 'a' AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn json_has_key_missing() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2}'::jsonb ? 'z' AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(false));
}

#[test]
fn json_has_any_key_match() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2, \"c\": 3}'::jsonb ?| ARRAY['a', 'z'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn json_has_any_key_no_match() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1}'::jsonb ?| ARRAY['x', 'y'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(false));
}

#[test]
fn json_has_all_keys_present() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2, \"c\": 3}'::jsonb ?& ARRAY['a', 'b'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn json_has_all_keys_missing_one() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"a\": 1, \"b\": 2}'::jsonb ?& ARRAY['a', 'z'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(false));
}

#[test]
fn json_has_key_on_empty_object() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT '{}'::jsonb ? 'a' AS v FROM t WHERE id = 1");
    assert_eq!(result.rows[0]["v"], Value::Bool(false));
}

#[test]
fn json_has_all_keys_on_single_key() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT '{\"x\": 10}'::jsonb ?& ARRAY['x'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Bool(true));
}

#[test]
fn json_each_returns_rows() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT * FROM json_each('{\"a\": 1, \"b\": 2}')");
    assert_eq!(result.rows.len(), 2);
    let keys: std::collections::BTreeSet<_> =
        result.rows.iter().map(|r| r["key"].clone()).collect();
    assert_eq!(keys, [s("a"), s("b")].into_iter().collect());
}

#[test]
fn json_each_key_value_pairs() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT key, value FROM json_each('{\"x\": 10, \"y\": 20}')",
    );
    let kv: BTreeMap<_, _> = result
        .rows
        .iter()
        .map(|r| (r["key"].clone(), r["value"].clone()))
        .collect();
    assert_eq!(kv[&s("x")], s("10"));
    assert_eq!(kv[&s("y")], s("20"));
}

#[test]
fn json_each_text_values_are_strings() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT * FROM json_each_text('{\"name\": \"Alice\", \"age\": \"30\"}')",
    );
    assert_eq!(result.rows.len(), 2);
    assert!(result
        .rows
        .iter()
        .all(|r| matches!(r["value"], Value::Str(_))));
}

#[test]
fn json_each_text_key_value_pairs() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT key, value FROM json_each_text('{\"k1\": \"v1\", \"k2\": \"v2\"}')",
    );
    let kv: BTreeMap<_, _> = result
        .rows
        .iter()
        .map(|r| (r["key"].clone(), r["value"].clone()))
        .collect();
    assert_eq!(kv[&s("k1")], s("v1"));
    assert_eq!(kv[&s("k2")], s("v2"));
}

#[test]
fn jsonb_each_returns_rows() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT * FROM jsonb_each('{\"p\": 100}')");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["key"], s("p"));
}

#[test]
fn json_array_elements_basic() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT * FROM json_array_elements('[1, 2, 3]')");
    assert_eq!(result.rows.len(), 3);
}

#[test]
fn json_array_elements_values() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT value FROM json_array_elements('[10, 20, 30]')",
    );
    let values: Vec<_> = result.rows.iter().map(|r| r["value"].clone()).collect();
    assert!(values.contains(&s("10")));
    assert!(values.contains(&s("20")));
    assert!(values.contains(&s("30")));
}

#[test]
fn json_array_elements_text_variant() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT * FROM json_array_elements_text('[\"a\", \"b\", \"c\"]')",
    );
    let values: Vec<_> = result.rows.iter().map(|r| r["value"].clone()).collect();
    assert_eq!(result.rows.len(), 3);
    assert!(values.contains(&s("a")));
    assert!(values.contains(&s("b")));
    assert!(values.contains(&s("c")));
}

#[test]
fn jsonb_array_elements_basic() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT * FROM jsonb_array_elements('[4, 5]')");
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn json_array_elements_single_element() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT * FROM json_array_elements('[42]')");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["value"], s("42"));
}

#[test]
fn json_array_elements_from_literal() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT value FROM json_array_elements('[\"python\", \"sql\", \"rust\"]')",
    );
    let values: Vec<_> = result.rows.iter().map(|r| r["value"].clone()).collect();
    assert_eq!(result.rows.len(), 3);
    assert!(values.contains(&s("python")));
    assert!(values.contains(&s("sql")));
    assert!(values.contains(&s("rust")));
}

#[test]
fn json_array_elements_integers() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT value FROM json_array_elements('[1, 2, 3]')",
    );
    assert_eq!(result.rows.len(), 3);
}
