//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_index_definition_preserves_defaults_and_strict_field_boundaries() {
    let full = serde_json::to_value(fixture()).unwrap();
    for name in full.as_object().unwrap().keys() {
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove(name);
        compare(&missing.to_string());
        let mut null = full.clone();
        null[name] = serde_json::Value::Null;
        compare(&null.to_string());
    }
    for path in [
        "/catalog",
        "/catalog/identity",
        "/relationships",
        "/column_order/0",
    ] {
        for name in full.pointer(path).unwrap().as_object().unwrap().keys() {
            let mut missing = full.clone();
            missing
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(name);
            compare(&missing.to_string());
        }
        let mut unknown = full.clone();
        unknown.pointer_mut(path).unwrap()["unknown"] = serde_json::json!(false);
        compare(&unknown.to_string());
    }
    for text in [
        r#"{"unique":false,"nulls_not_distinct":false,"unique":false}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"unknown":null}"#,
        r#"{"column_order":[{"descending":false,"nulls_first":false,"unknown":"\ud800"}],"unique":false,"nulls_not_distinct":false}"#,
        r#"{"relationships":{"parent_index":null,"parent_index":null},"unique":false,"nulls_not_distinct":false}"#,
        r"[null,{},[],[],[],[],null,false]",
    ] {
        compare(text);
    }
    let reversed = full
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(key, value)| format!("{key:?}:{value}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(compare(&format!("{{{reversed}}}")));
}

#[test]
fn stored_index_definition_preserves_numeric_classification_and_nested_depth() {
    let source = serde_json::to_string(&fixture()).unwrap();
    for number in ["-0", "0.0", "0e0", "4294967295", "4294967296"] {
        compare(&source.replace("40001", number));
        compare(&format!(
            r#"{{"predicate":{{"Param":{number}}},"unique":false,"nulls_not_distinct":false}}"#
        ));
    }
    for depth in [0, 1, 121, 122, 123, 124, 125, 126, 127] {
        let expression = format!(
            r#"{}{{"Literal":true}}{}"#,
            r#"{"Not":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&format!(
            r#"{{"predicate":{expression},"unique":false,"nulls_not_distinct":false}}"#
        ));
        let ty = format!(
            r#"{}"Integer"{}"#,
            r#"{"Array":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&format!(
            r#"{{"key_types":[{ty}],"unique":false,"nulls_not_distinct":false}}"#
        ));
    }
}

#[test]
fn stored_index_definition_matches_native_truncation_and_mutation_rejection() {
    let source = serde_json::to_string(&fixture()).unwrap();
    for boundary in 0..source.len() {
        compare(&source[..boundary]);
    }
    for (index, byte) in source.bytes().enumerate() {
        for replacement in [b'"', b':', b',', b'{', b']', b'0', b'x'] {
            if byte == replacement {
                continue;
            }
            let mut changed = source.as_bytes().to_vec();
            changed[index] = replacement;
            compare(std::str::from_utf8(&changed).unwrap());
        }
    }
}
