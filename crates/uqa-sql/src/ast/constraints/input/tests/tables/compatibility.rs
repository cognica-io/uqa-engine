//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_table_constraints_preserve_field_defaults_and_strict_metadata() {
    let full = fixture();
    for name in full.as_object().unwrap().keys() {
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove(name);
        compare(&missing.to_string());
        let mut null = full.clone();
        null[name] = serde_json::Value::Null;
        compare(&null.to_string());
    }
    for field in [
        "checks",
        "key_constraints",
        "foreign_keys",
        "dropped_attributes",
    ] {
        for name in full[field][0].as_object().unwrap().keys() {
            let mut missing = full.clone();
            missing[field][0].as_object_mut().unwrap().remove(name);
            compare(&missing.to_string());
        }
    }
    for text in [
        r#"{"persistence":"Permanent","persistence":"Unlogged"}"#,
        r#"{"unknown":"\ud800"}"#,
        r#"{"catalog_oids":{"relation":1,"unknown":false}}"#,
        r#"{"hierarchy":{"partition_identity_overrides":[{"column":"x","original":true}]}}"#,
        r#"{"hierarchy":{"partition_identity_overrides":[{"column":"x","original":{"kind":"serial","unknown":"\ud800"}}]}}"#,
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
fn stored_table_constraints_preserve_nested_depth_and_direct_numeric_rules() {
    let identity = r#"{"hierarchy":{"partition_identity_overrides":[{"column":"x","original":{"kind":"serial","declaration":{"sequence":{"start":{"Integer":NUMBER}}}}}]}}"#;
    assert!(compare(&identity.replace("NUMBER", "1")));
    for number in ["-0", "0.0", "0e0", "-2147483649", "2147483648"] {
        compare(&format!(
            r#"{{"hierarchy":{{"partition_bound":{{"Hash":{{"modulus":{number},"remainder":0}}}}}}}}"#
        ));
        compare(&identity.replace("NUMBER", number));
        compare(&format!(
            r#"{{"checks":[{{"expr":{{"Param":{number}}}}}]}}"#
        ));
    }
    for depth in [0, 1, 120, 121, 122, 123, 124, 125, 126, 127] {
        let expression = format!(
            r#"{}{{"Literal":true}}{}"#,
            r#"{"Not":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&format!(
            r#"{{"hierarchy":{{"partition_bound":{{"List":[{expression}]}}}}}}"#
        ));
        compare(&format!(r#"{{"checks":[{{"expr":{expression}}}]}}"#));
    }
}

#[test]
fn stored_table_constraints_match_native_truncation_and_mutation_rejection() {
    let full = fixture().to_string();
    for boundary in 0..full.len() {
        compare(&full[..boundary]);
    }
    for (index, byte) in full.bytes().enumerate() {
        for replacement in [b'"', b':', b',', b'{', b']', b'0', b'x'] {
            if byte == replacement {
                continue;
            }
            let mut changed = full.as_bytes().to_vec();
            changed[index] = replacement;
            compare(std::str::from_utf8(&changed).unwrap());
        }
    }
}
