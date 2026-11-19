//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_schema_input_preserves_field_defaults_order_and_unknown_rules() {
    for text in STRUCTS {
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(text).unwrap();
        let (tag, payload) = object.into_iter().next().unwrap();
        let fields = payload.as_object().unwrap();
        for name in fields.keys() {
            let mut missing = fields.clone();
            missing.remove(name);
            compare(&format!(
                r#"{{"{tag}":{}}}"#,
                serde_json::to_string(&missing).unwrap()
            ));
            let mut null = fields.clone();
            null.insert(name.clone(), serde_json::Value::Null);
            compare(&format!(
                r#"{{"{tag}":{}}}"#,
                serde_json::to_string(&null).unwrap()
            ));
        }
        let reversed = fields
            .iter()
            .rev()
            .map(|(name, value)| format!("{name:?}:{value}"))
            .collect::<Vec<_>>()
            .join(",");
        compare(&format!(r#"{{"{tag}":{{{reversed},"unknown":"\ud800"}}}}"#));
    }
    for text in [
        r#"{"Binary":["Equal",{"Column":"x"},{"Literal":1}]}"#,
        r#"{"Case":[null,[[{"Literal":true},{"Literal":1}]],null]}"#,
        r#"{"Func":["f",null,[],false,[],"Legacy",null]}"#,
        r#"{"CompositeRow":{"items":[],"binding":["s.t",[]]}}"#,
        r#"{"CompositeRow":{"items":[],"binding":{"ty":"s.t","attributes":[],"unknown":null}}}"#,
        r#"{"Cast":{"expr":{"Literal":1},"ty":"text"}}"#,
        r#"{"Cast":{"expr":{"Literal":1},"ty":"text","expr":"Star"}}"#,
    ] {
        compare(text);
    }
}

#[test]
fn stored_schema_input_preserves_depth_and_integer_content_boundaries() {
    for number in [
        "-0",
        "0.0",
        "0e0",
        "-1",
        "18446744073709551615",
        "18446744073709551616",
    ] {
        compare(&format!(r#"{{"Param":{number}}}"#));
        compare(&format!(
            r#"{{"InternalColumn":{{"relation":{number},"attribute":0}}}}"#
        ));
    }
    for depth in [0, 1, 15, 16, 64, 124, 125, 126, 127, 128] {
        let text = format!(
            r#"{}{{"Literal":true}}{}"#,
            r#"{"Not":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&text);
        let literal = format!(
            r#"{{"Literal":{}null{}}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        );
        compare(&literal);
    }
}

#[test]
fn stored_schema_input_matches_native_truncation_and_byte_mutation_rejection() {
    for text in [CALL, ROW, TYPED] {
        for boundary in 0..text.len() {
            compare(&text[..boundary]);
        }
        for (index, byte) in text.bytes().enumerate() {
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
