//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_columns_json_preserves_field_defaults_duplicates_and_order() {
    let full = complete();
    for name in full[0].as_object().unwrap().keys() {
        let mut missing = full.clone();
        missing[0].as_object_mut().unwrap().remove(name);
        compare(&missing.to_string());
        let mut null = full.clone();
        null[0][name] = serde_json::Value::Null;
        compare(&null.to_string());
    }
    let reversed = full[0]
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(key, value)| format!("{key:?}:{value}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(compare(&format!("[{{{reversed}}}]")));
    for extra in [
        r#""name":"duplicate""#,
        r#""unknown":"\ud800""#,
        r#""unknown":{"deep":[true]}""#,
    ] {
        compare(&SIMPLE.replace(r#""name":"column""#, &format!(r#"{extra},"name":"column""#)));
    }
    for path in [
        "auto_increment",
        "generated",
        "references",
        "not_null_identity",
    ] {
        let fields = full[0][path].as_object().unwrap();
        for name in fields.keys() {
            let mut missing = full.clone();
            missing[0][path].as_object_mut().unwrap().remove(name);
            compare(&missing.to_string());
        }
    }
}

#[test]
fn stored_columns_json_preserves_nested_depth_and_numeric_content_rules() {
    for number in [
        "-0",
        "0.0",
        "0e0",
        "-32769",
        "32768",
        "18446744073709551616",
    ] {
        compare(&SIMPLE.replace(
            r#""name":"column""#,
            &format!(r#""attribute_number":{number},"name":"column""#),
        ));
        compare(&SIMPLE.replace(r#""name":"column""#, &format!(r#""auto_increment":{{"kind":"serial","declaration":{{"sequence":{{"start":{{"Integer":{number}}}}}}}}},"name":"column""#)));
        compare(&SIMPLE.replace(
            r#""name":"column""#,
            &format!(r#""default":{{"Param":{number}}},"name":"column""#),
        ));
    }
    for depth in [0, 1, 63, 122, 123, 124, 125, 126, 127, 128] {
        let expression = format!(
            r#"{}{{"Literal":true}}{}"#,
            r#"{"Not":"#.repeat(depth),
            "}".repeat(depth)
        );
        compare(&SIMPLE.replace(
            r#""name":"column""#,
            &format!(r#""default":{expression},"name":"column""#),
        ));
        let value = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        compare(&SIMPLE.replace(
            r#""name":"column""#,
            &format!(r#""missing_value":{value},"name":"column""#),
        ));
    }
    compare(&SIMPLE.replace(
        r#""name":"column""#,
        r#""auto_increment":{"kind":"serial","unknown":"\ud800"},"name":"column""#,
    ));
}

#[test]
fn stored_columns_json_matches_native_truncation_and_mutation_rejection() {
    let full = complete().to_string();
    for text in [SIMPLE, full.as_str()] {
        for boundary in 0..text.len() {
            if text.is_char_boundary(boundary) {
                compare(&text[..boundary]);
            }
        }
        for (index, byte) in text.bytes().enumerate() {
            if !byte.is_ascii() {
                continue;
            }
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
