//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sequences(expression: &Value, path: &[&str], names: &str) {
    let mut object = expression;
    for name in path {
        object = &object[*name];
    }
    let items = names
        .split_whitespace()
        .map(|name| object[name].clone())
        .collect::<Vec<_>>();
    for length in 0..=items.len() + 1 {
        let mut changed = expression.clone();
        let mut slot = &mut changed;
        for name in path {
            slot = &mut slot[*name];
        }
        let mut selected = items[..length.min(items.len())].to_vec();
        if length > items.len() {
            selected.push(Value::Null);
        }
        *slot = selected.into();
        compare(&changed.to_string());
    }
}

#[test]
fn stored_query_input_preserves_positional_defaults_and_internal_source_types() {
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
    sequences(
        &expression,
        &["ScalarSubquery"],
        "windows projections values from where group_by grouping_sets group_distinct having order_by limit with_ties offset with set_op distinct distinct_on locking",
    );
    let source = &mut expression["ScalarSubquery"]["from"]["Values"];
    source["internal_relation"] = json!(u64::MAX);
    source["internal_column_types"] = json!([null, "Text", {"Array":{"Domain":{"schema":"s","name":"d","oid":40000,"array_oid":40001,"base":"Integer"}}}]);
    source["internal_column_collations"] = json!([null, 100, u32::MAX]);
    assert!(compare(&expression.to_string()));
    mutate(&expression, &mut |value| {
        compare(&value.to_string());
    });
    let window = json!({"WindowCall":{"name":"sum","args":[],"spec":{"definition":0,"reference":{"name":"w","kind":"Copy"},"partition_by":[],"order_by":[],"frame":null},"filter":null,"modifiers":{"distinct":false,"ordered":false,"within_group":false}}});
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
fn stored_query_input_preserves_merge_identities_and_retired_replacements() {
    let mut expression = query(QUERIES[11]);
    let bindings = &mut expression["ScalarSubquery"]["with"][0]["merge"]["target_column_bindings"];
    *bindings = json!({"removed": {"object_id":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"attribute_number":3,"domain_dependencies":[40000,40001,40000]}});
    assert!(compare(&expression.to_string()));
    mutate(&expression, &mut |value| {
        compare(&value.to_string());
    });
    let text = expression.to_string();
    let original = bindings_text(&expression);
    let duplicate = format!("{{\"removed\":{original},\"removed\":{original}}}");
    let encoded = text.replace(
        &expression["ScalarSubquery"]["with"][0]["merge"]["target_column_bindings"].to_string(),
        &duplicate,
    );
    assert!(compare(&encoded));
}

fn bindings_text(expression: &Value) -> String {
    expression["ScalarSubquery"]["with"][0]["merge"]["target_column_bindings"]["removed"]
        .to_string()
}
