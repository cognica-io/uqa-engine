//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::{json, Value};

// These field orders are taken from the original stored AST declarations. The oracle below is their unchanged Serde implementation, independently of the retained reader's grammar table.
const SELECT_FIELDS: &str = "windows projections values from where group_by grouping_sets group_distinct having order_by limit with_ties offset with set_op distinct distinct_on locking";

fn at_path(value: &Value, path: &[&str], replacement: Value) -> Value {
    let mut value = value.clone();
    let mut slot = &mut value;
    for key in path {
        slot = &mut slot[*key];
    }
    *slot = replacement;
    value
}

fn sequences(expression: &Value, path: &[&str], order: &str) {
    let mut object = expression;
    for key in path {
        object = &object[*key];
    }
    let fields = order.split_whitespace().collect::<Vec<_>>();
    let items = fields
        .iter()
        .map(|key| object[*key].clone())
        .collect::<Vec<_>>();
    // Every suffix length includes both exact and missing/defaulted arities. Extra items must retain their original rejection.
    for length in 0..=items.len() {
        compare(&keys(at_path(
            expression,
            path,
            items[..length].to_vec().into(),
        )));
    }
    let mut extra = items;
    extra.push(Value::Null);
    assert!(!compare(&keys(at_path(expression, path, extra.into()))));
}

#[test]
fn stored_index_columns_preserve_query_sequences_and_bound_source_metadata() {
    let mut expression = query("SELECT sum(a) OVER () FROM (VALUES (1)) AS t(a)");
    let select = &mut expression["ScalarSubquery"];
    for (name, value) in [
        ("windows", json!([])),
        ("values", json!([])),
        ("group_distinct", json!(false)),
        ("with_ties", json!(false)),
        ("locking", json!([])),
    ] {
        select.as_object_mut().unwrap().entry(name).or_insert(value);
    }
    sequences(&expression, &["ScalarSubquery"], SELECT_FIELDS);
    let values = &mut expression["ScalarSubquery"]["from"]["Values"];
    values["internal_relation"] = json!(u64::MAX);
    values["internal_column_types"] = json!([null, "Text", {"Array":{"Domain":{"schema":"s","name":"d","oid":40000,"array_oid":40001,"base":"Integer"}}}]);
    values["internal_column_collations"] = json!([null, 100, u32::MAX]);
    assert!(compare(&keys(expression.clone())));
    mutate(&expression, &mut |value| {
        compare(&keys(value));
    });
    let window = json!({"WindowCall":{"name":"sum","args":[],"spec":{"definition":0,"reference":{"name":"w","kind":"Copy"},"partition_by":[],"order_by":[],"frame":null},"filter":null,"modifiers":{"distinct":false,"ordered":false,"within_group":false}}});
    assert!(compare(&keys(window.clone())));
    sequences(&window, &["WindowCall"], "name args spec filter modifiers");
    sequences(
        &window,
        &["WindowCall", "spec"],
        "definition reference partition_by order_by frame",
    );
    sequences(
        &window,
        &["WindowCall", "modifiers"],
        "distinct ordered within_group",
    );
}

#[test]
fn stored_index_columns_validate_flattened_ctes_and_untagged_assignment_fallbacks() {
    let mut expression = query(QUERIES[9]);
    let multiple = expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0].clone();
    assert!(multiple.get("targets").is_some());
    // A failed single partial target must still permit the valid multiple-target alternative. A valid first alternative does not validate unrelated fields from the later alternative.
    let mut alternatives = multiple.as_object().unwrap().clone();
    alternatives.insert("column".into(), json!("ignored_first"));
    for indirection in [json!(false), json!([{"Index":{"Unknown":null}}]), json!([])] {
        alternatives.insert("indirection".into(), indirection);
        expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0] =
            Value::Object(alternatives.clone());
        assert!(compare(&keys(expression.clone())));
    }
    alternatives.insert("targets".into(), json!(false));
    expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0] = alternatives.into();
    assert!(compare(&keys(expression)));

    let minimal = query("SELECT 1")["ScalarSubquery"].clone();
    let encoded = serde_json::to_string(&minimal).unwrap();
    let outer = keys(query("WITH q AS (SELECT 1) SELECT 2"));
    let original_cte =
        serde_json::to_string(&query("WITH q AS (SELECT 1) SELECT 2")["ScalarSubquery"]["with"][0])
            .unwrap();
    for (body, accepted) in [
        (format!("\"query\":{encoded},\"insert\":false"), true),
        (format!("\"query\":{encoded},\"query\":false"), true),
        (format!("\"insert\":false,\"query\":{encoded}"), false),
        (format!("\"query\":false,\"query\":{encoded}"), false),
        (format!("\"unknown\":0,\"query\":{encoded}"), true),
        (format!("\"query\":{encoded},\"name\":\"duplicate\""), false),
    ] {
        let cte = format!("{{\"name\":\"q\",\"columns\":[],\"recursive\":false,{body}}}");
        assert_eq!(compare(&outer.replace(&original_cte, &cte)), accepted);
    }
}

#[test]
fn stored_index_columns_preserve_original_depth_unicode_and_number_classes() {
    for depth in [0, 1, 123, 124, 125, 126, 127, 128] {
        let text = format!(
            "[{}{{\"Literal\":0}}{}]",
            "{\"Not\":".repeat(depth),
            "}".repeat(depth)
        );
        compare(&text);
        let unknown = format!("[{{\"WindowCall\":{{\"name\":\"x\",\"args\":[],\"spec\":{{\"partition_by\":[],\"order_by\":[]}},\"unknown\":{}0{}}}}}]", "[".repeat(depth), "]".repeat(depth));
        compare(&unknown);
    }
    for value in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1e0",
        "1e99999",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
    ] {
        compare(&format!("[{{\"Param\":{value}}}]"));
        compare(&format!("[{{\"Literal\":{value}}}]"));
        compare(&format!("[{{\"WindowCall\":{{\"name\":\"x\",\"args\":[],\"spec\":{{\"partition_by\":[],\"order_by\":[],\"definition\":{value}}}}}}}]"));
    }
    for text in [
        r#"["\uD834\uDD1E","한글\u0000",{"Star":null},"Default"]"#,
        r#"["\uD800"]"#,
        r#"[{"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[]},"unused":"\uD800"}}]"#,
        r#"[{"WindowCall":{"name":"x","args":[],"args":[],"spec":{"partition_by":[],"order_by":[]}}}]"#,
        r#"[{"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[]},"unused":1,"unused":2}}]"#,
    ] {
        compare(text);
    }
    let text = format!(
        "[{}{{\"Literal\":true}}{}]",
        "{\"Not\":".repeat(125),
        "}".repeat(125)
    );
    // The ordinary Serde oracle is evaluated on this test thread, outside the small retained-input stack.
    assert!(serde_json::from_str::<Vec<IndexKey>>(&text).is_ok());
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut read = input(text.into(), &memory, &token);
            assert!(finish(&mut read, 1).unwrap().is_empty());
            drop(read);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn stored_index_columns_preserve_optional_command_variants_and_numeric_content() {
    let mut expression = query(QUERIES[11]);
    for tag in [
        "DeleteNotMatchedBySource",
        "NothingMatched",
        "NothingNotMatched",
        "NothingNotMatchedBySource",
    ] {
        for body in [
            json!({}),
            json!({"condition":null}),
            json!({"condition":{"Literal":true}}),
            json!([]),
            json!(null),
            json!(-0.0),
        ] {
            expression["ScalarSubquery"]["with"][0]["merge"]["when_clauses"] = json!([{tag:body}]);
            compare(&keys(expression.clone()));
        }
    }
    let mut expression = query(QUERIES[8]);
    for action in [
        json!("Nothing"),
        json!({"Nothing":null}),
        json!({"Nothing":false}),
    ] {
        expression["ScalarSubquery"]["with"][0]["insert"]["on_conflict"]["action"] = action;
        compare(&keys(expression.clone()));
    }
    let mut expression = query("SELECT * FROM generate_series(1, 3) AS q(a)");
    let function = &mut expression["ScalarSubquery"]["from"]["Function"];
    function["relations"] = json!({"left":"s.a","right":"s.b"});
    function["binding"] = json!({"name":"f","argument_types":[]});
    assert!(compare(&keys(expression.clone())));
    mutate(&expression, &mut |value| {
        compare(&keys(value));
    });
    for number in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1e0",
        "1e99999",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
    ] {
        let original = keys(query(QUERIES[11]));
        let needle = serde_json::to_string(
            &query(QUERIES[11])["ScalarSubquery"]["with"][0]["merge"]["when_clauses"],
        )
        .unwrap();
        let changed = original.replace(&needle, &format!("[{{\"NothingMatched\":{number}}}]"));
        compare(&changed);
    }
}
