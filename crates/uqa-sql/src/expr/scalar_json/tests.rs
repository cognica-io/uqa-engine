//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn json(text: &str) -> Value {
    Value::Json(text.into())
}
fn jsonb(text: &str) -> Value {
    Value::JsonB(text.into())
}
fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn assert_value(name: &str, args: &[Value], expected: Value) {
    let memory = MemoryBudget::new(4 * 1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let result = eval_json_functions_with_control(name, args, &control)
        .expect("controlled immutable JSON family")
        .unwrap();
    assert_eq!(&*result, &expected, "{name}");
    assert_eq!(
        memory.used(),
        result.reserved_bytes(),
        "scratch released for {name}"
    );
    if let Value::Str(value) | Value::Json(value) | Value::JsonB(value) = &*result {
        assert_eq!(memory.used(), value.capacity());
    }
    assert_eq!(eval_json_functions(name, args).unwrap().unwrap(), expected);
    drop(result);
    assert_eq!(memory.used(), 0, "result release for {name}");
}

#[test]
fn immutable_json_reads_preserve_type_extraction_numeric_and_containment_rules() {
    for name in ["json_typeof", "jsonb_typeof"] {
        assert_value(name, &[json(r#"{"x":null}"#)], text("object"));
        assert_value(name, &[text("not JSON")], text("string"));
        assert_value(name, &[Value::Null], text("null"));
    }
    for name in ["json_array_length", "jsonb_array_length"] {
        assert_value(name, &[json("[1,null,[]]")], Value::Int(3));
    }
    for (name, expected) in [
        ("json_extract_path", json("1.00")),
        ("jsonb_extract_path", jsonb("1.00")),
        ("json_extract_path_text", text("1.00")),
        ("jsonb_extract_path_text", text("1.00")),
    ] {
        assert_value(
            name,
            &[
                json(r#"{"a":[0,1.00],"a":[false,1.00]}"#),
                text("a"),
                text("-1"),
            ],
            expected,
        );
    }
    assert_value(
        "json_extract_path_text",
        &[json(r#"{"a":null}"#), text("a")],
        Value::Null,
    );
    assert_value(
        "json_extract_path",
        &[json("bad"), Value::Null],
        Value::Null,
    );
    assert_value(
        "json_contains",
        &[json("[100,0]"), json("[1e2,-0.0]")],
        Value::Bool(true),
    );
    assert_value(
        "json_contains",
        &[json("[[1]]"), json("1")],
        Value::Bool(false),
    );
    assert_value(
        "json_contains",
        &[json("1e9223372036854775808"), json("1e9223372036854775808")],
        Value::Bool(false),
    );
    assert_value(
        "json_contained_by",
        &[json(r#"{"a":1}"#), json(r#"{"a":1.00,"b":2}"#)],
        Value::Bool(true),
    );
    assert_value(
        "json_has_key",
        &[json(r#"["a",1]"#), text("a")],
        Value::Bool(true),
    );
    assert_value(
        "json_has_any_key",
        &[
            json(r#"{"a":1}"#),
            Value::List(vec![text("z"), Value::List(vec![text("a")])]),
        ],
        Value::Bool(true),
    );
    assert_value(
        "json_has_all_keys",
        &[
            json(r#"["a","b"]"#),
            Value::List(vec![text("a"), text("b")]),
        ],
        Value::Bool(true),
    );
    assert_value(
        "json_has_all_keys",
        &[json("null"), Value::List(vec![])],
        Value::Bool(true),
    );
}

#[test]
fn immutable_json_mutations_keep_lexical_json_and_canonical_jsonb_results() {
    assert_value(
        "jsonb_set",
        &[jsonb("{}"), text("{a,b}"), json("1.00")],
        jsonb(r#"{"a": {"b": 1.00}}"#),
    );
    assert_value(
        "jsonb_set",
        &[jsonb("[1]"), text("{3}"), json("2")],
        jsonb("[1, null, null, 2]"),
    );
    assert_value(
        "jsonb_set",
        &[
            jsonb("{}"),
            text("{a}"),
            text("unparsed"),
            Value::Bool(false),
        ],
        jsonb("{}"),
    );
    assert_value(
        "jsonb_insert",
        &[jsonb("[1,2]"), text("{-1}"), json("3"), Value::Bool(true)],
        jsonb("[1, 2, 3]"),
    );
    assert_value(
        "jsonb_insert",
        &[jsonb(r#"{"a":1}"#), text("{a}"), json("2")],
        jsonb(r#"{"a": 1}"#),
    );
    assert_value(
        "json_delete_path",
        &[
            jsonb(r#"{"a":[1,2,3]}"#),
            Value::List(vec![text("a"), text("-2")]),
        ],
        jsonb(r#"{"a": [1, 3]}"#),
    );
    let source = r#"{"z":1.2300e+02,"a":null,"z":2,"s":"\u0061","n":[null,{"x":null}]}"#;
    assert_value(
        "json_strip_nulls",
        &[json(source), Value::Bool(true)],
        json(r#"{"z":1.2300e+02,"z":2,"s":"a","n":[{}]}"#),
    );
    assert_value(
        "jsonb_strip_nulls",
        &[json(source), Value::Bool(true)],
        jsonb(r#"{"n": [{}], "s": "a", "z": 2}"#),
    );
    assert_value(
        "jsonb_strip_nulls",
        &[json(r#"{"x":1,"x":null}"#)],
        jsonb("{}"),
    );
    assert_value("json_strip_nulls", &[json("bad"), Value::Null], Value::Null);
    assert_value(
        "jsonb_pretty",
        &[jsonb(r#"{"zz":1,"b":[],"aa":{"long":3,"x":2}}"#)],
        text(
            "{\n    \"b\": [\n    ],\n    \"aa\": {\n        \"x\": 2,\n        \"long\": 3\n    },\n    \"zz\": 1\n}",
        ),
    );
    assert_value("jsonb_pretty", &[json("1e200000")], text("1e+200000"));
}

#[test]
fn immutable_jsonpath_keeps_selector_predicate_and_alias_behavior() {
    let source = json(r#"{"a":[{"x":1},{"x":3}],"truth":true,"name":"é"}"#);
    for name in ["jsonb_path_exists", "jsonpath_exists"] {
        assert_value(
            name,
            &[source.clone(), text("lax $.a[*] ? (@.x >= 3)")],
            Value::Bool(true),
        );
        assert_value(
            name,
            &[source.clone(), text("$.a[*] ? (@.x < 0)")],
            Value::Bool(false),
        );
        assert_value(
            name,
            &[source.clone(), text("$.missing")],
            Value::Bool(false),
        );
    }
    for name in ["jsonb_path_match", "jsonpath_match"] {
        assert_value(
            name,
            &[source.clone(), text("strict $.a[-1].x == 3.0")],
            Value::Bool(true),
        );
        assert_value(name, &[source.clone(), text("$.truth")], Value::Bool(true));
        assert_value(
            name,
            &[source.clone(), text("$.name == \"é\"")],
            Value::Bool(true),
        );
        assert_value(
            name,
            &[json("1e200000"), text("@ == 1e200000")],
            Value::Bool(false),
        );
    }
}

#[test]
fn immutable_json_failures_release_scratch_and_check_both_cancellation_owners() {
    let cases = [
        (
            "json_extract_path",
            vec![json(r#"{"a":[1,}"#), text("a")],
            "22P02",
        ),
        (
            "jsonb_set",
            vec![jsonb("{}"), text("{a}"), json("1e200000")],
            "22003",
        ),
        ("json_strip_nulls", vec![json(r#"{"a":"\uD800"}"#)], "22P02"),
    ];
    for (name, args, state) in cases {
        let memory = MemoryBudget::new(1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        assert_eq!(
            eval_json_functions_with_control(name, &args, &control)
                .unwrap()
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        for (name, args) in [
            ("json_typeof", vec![Value::Null]),
            ("json_extract_path", vec![Value::Null, text("a")]),
            ("json_strip_nulls", vec![Value::Null]),
            ("jsonpath_match", vec![json("true"), text("$")]),
        ] {
            assert_eq!(
                eval_json_functions_with_control(name, &args, &control)
                    .unwrap()
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [0, 256, 1024] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let error = eval_json_functions_with_control(
            "jsonb_set",
            &[jsonb("[1]"), text("{4096}"), json("2")],
            &control,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn controlled_json_operators_preserve_key_types_paths_and_optional_dispatch() {
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let result = json::json_extract_operator_with_control(
        &[json(r#"{"0":"object"}"#), Value::Int(0)],
        true,
        false,
        &control,
    )
    .unwrap();
    assert_eq!(&*result, &Value::Null);
    drop(result);
    let result = json::json_extract_operator_with_control(
        &[jsonb(r#"{"a":[1,2]}"#), text("{a,-1}")],
        true,
        true,
        &control,
    )
    .unwrap();
    assert_eq!(&*result, &text("2"));
    drop(result);
    let result =
        json::json_concat_with_control(&[jsonb(r#"{"a":1}"#), jsonb(r#"{"a":2,"b":3}"#)], &control)
            .unwrap()
            .unwrap();
    assert_eq!(&*result, &jsonb(r#"{"a": 2, "b": 3}"#));
    drop(result);
    let result = json::json_delete_with_control(&[jsonb(r#"["a",1,"a",2]"#), text("a")], &control)
        .unwrap()
        .unwrap();
    assert_eq!(&*result, &jsonb("[1, 2]"));
    drop(result);
    assert!(
        json::json_concat_with_control(&[text("a"), text("b")], &control)
            .unwrap()
            .is_none()
    );
    assert!(
        json::json_delete_with_control(&[Value::Int(1), Value::Int(2)], &control)
            .unwrap()
            .is_none()
    );
    assert_eq!(memory.used(), 0);
}
