//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_call_input_preserves_generic_content_and_nested_type_depth() {
    for depth in [0, 1, 15, 16, 64, 123, 124, 125, 126, 127, 128, 129] {
        let arrays = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        compare(&binding(&format!(r#""unknown":{arrays}"#)));
        let ty = format!(
            r#"{}"Text"{}"#,
            r#"{"Array":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&binding(&format!(
            r#""composite_field":{{"type_oid":42,"number":1,"result_type":{ty},"dropped":false}}"#
        )));
    }
}

#[test]
fn stored_call_input_preserves_unknown_duplicate_optional_and_flattened_fields() {
    for fields in [
        r#""unknown":{"anything":[1e9999,"\ud83d\ude03",null]}"#,
        r#""unknown":"\ud800""#,
        r#""name":"duplicate""#,
        r#""invocation":null,"invocation":null"#,
        r#""dispatch":null,"builtin":null"#,
        r#""object_id":[0]"#,
        r#""object_id":null,"dispatch":null,"input_collation":null"#,
        r#""composite_field":null"#,
        r#""assignment":{}"#,
        r#""assignment":{"path":[],"value_argument":0},"composite_field":null"#,
        r#""composite_field":null,"assignment":{"path":[],"value_argument":0}"#,
        r#""assignment":{"path":[],"value_argument":1},"assignment":{"path":[],"value_argument":2}"#,
        r#""composite_field":[1234,2,"Text",null,false]"#,
        r#""composite_field":{"type_oid":1234,"number":2,"result_type":"Text","dropped":false,"future":"\ud800"}"#,
    ] {
        compare(&binding(fields));
    }
    for text in [
        "null",
        "[]",
        r#"[null,"f",[]]"#,
        r#"{"name":"f"}"#,
        r#"{"argument_types":[]}"#,
    ] {
        assert!(!compare(text));
    }
}

#[test]
fn stored_call_input_matches_native_json_field_and_integer_rules() {
    let object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(COMPLETE).unwrap();
    for field in object.keys() {
        let mut missing = object.clone();
        missing.remove(field);
        compare(&serde_json::to_string(&missing).unwrap());
        let mut null = object.clone();
        null.insert(field.clone(), serde_json::Value::Null);
        compare(&serde_json::to_string(&null).unwrap());
    }
    for number in [
        "-0",
        "0.0",
        "0e0",
        "4294967295",
        "4294967296",
        "-1",
        "18446744073709551615",
        "18446744073709551616",
        "1e9999",
    ] {
        compare(&binding(&format!(
            r#""dispatch":{{"TypeOf":{{"type_oid":{number}}}}}"#
        )));
        compare(&binding(&format!(
            r#""assignment":{{"path":[],"value_argument":{number}}}"#
        )));
    }
}

#[test]
fn stored_call_input_matches_native_rejection_at_every_truncation_and_mutation() {
    for text in [COMPLETE.to_owned(), binding(ASSIGNMENT)] {
        for boundary in 0..text.len() {
            if text.is_char_boundary(boundary) {
                compare(&text[..boundary]);
            }
        }
        for (index, byte) in text
            .bytes()
            .enumerate()
            .filter(|(_, byte)| byte.is_ascii() && !byte.is_ascii_whitespace())
        {
            for replacement in [b'"', b':', b',', b'{', b']', b'0', b'x'] {
                if byte == replacement {
                    continue;
                }
                let mut changed = text.as_bytes().to_vec();
                changed[index] = replacement;
                compare(std::str::from_utf8(&changed).unwrap());
            }
        }
    }
}
