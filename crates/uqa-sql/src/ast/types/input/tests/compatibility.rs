//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_type_fields_preserve_maps_sequences_defaults_and_duplicate_errors() {
    for &(tag, payload, names) in STRUCTS {
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(payload).unwrap();
        let fields: Vec<_> = names
            .iter()
            .map(|name| format!("{name:?}:{}", object[*name]))
            .collect();
        let sequence: Vec<_> = names.iter().map(|name| object[*name].to_string()).collect();
        assert!(compare(&tagged(tag, &format!("[{}]", sequence.join(",")))));
        for position in 0..names.len() {
            let mut missing = object.clone();
            missing.remove(names[position]);
            compare(&tagged(tag, &serde_json::to_string(&missing).unwrap()));
            let mut duplicate = fields.clone();
            duplicate.insert(position, fields[position].clone());
            assert!(!compare(&tagged(
                tag,
                &format!("{{{}}}", duplicate.join(","))
            )));
            compare(&tagged(
                tag,
                &format!("[{}]", sequence[..position].join(",")),
            ));
        }
        assert!(!compare(&tagged(
            tag,
            &format!("[{},null]", sequence.join(","))
        )));
        let mut reverse = fields.clone();
        reverse.reverse();
        assert!(compare(&tagged(tag, &format!("{{{}}}", reverse.join(",")))));
        for unknown in [
            "null",
            "1e400",
            "[]",
            "{}",
            r#""\ud800""#,
            r#"{"\ud800":[null,true]}"#,
        ] {
            assert!(compare(&tagged(
                tag,
                &format!(
                    "{{\"ignored\":{unknown},{},\"ignored\":{unknown}}}",
                    fields.join(",")
                )
            )));
        }
    }
    assert!(compare(r#"{"Numeric":{}}"#));
    assert!(compare(
        r#"{"Domain":{"schema":"s","name":"n","oid":0,"base":"Text"}}"#
    ));
    assert!(!compare(
        r#"{"Domain":{"schema":"s","sch\u0065ma":"s","name":"n","oid":0,"base":"Text"}}"#
    ));
}

#[test]
fn stored_type_numbers_and_unit_tags_match_the_public_json_decoder() {
    for number in [
        "0",
        "-0",
        "-1",
        "1",
        "4294967295",
        "4294967296",
        "-2147483648",
        "-2147483649",
        "2147483647",
        "2147483648",
        "1.0",
        "1e0",
        "1e400",
        "01",
        "--1",
        "NaN",
        "null",
        "true",
        "\"1\"",
        "[]",
        "{}",
    ] {
        for tag in ["Varchar", "Character", "Bit", "VarBit", "Tensor"] {
            compare(&tagged(tag, number));
        }
        compare(&tagged("Numeric", &format!("{{\"scale\":{number}}}")));
    }
    for text in [
        "null",
        "[]",
        "{}",
        "0",
        "true",
        r#""Unknown""#,
        r#""Array""#,
        r#"{"Integer":null,"Text":null}"#,
        r#"{"Array":null}"#,
        r#"{"Range":"Void"}"#,
        r#"{"Range":{"Integer":null,"Date":null}}"#,
        r#"{"IntervalWithFields":{"fields":"bad"}}"#,
        r#""\u0049nteger""#,
        r#"{"\u0049nteger":null}"#,
        r#"{"Named":"\ud800"}"#,
        r#"{"Enum":{"schema":"\ud800","name":"n","oid":1,"array_oid":2}}"#,
        r#"{"Numeric":{"\ud800":null}}"#,
    ] {
        compare(text);
    }
    let enormous = "9".repeat(1 << 18);
    assert!(!compare(&tagged("Character", &enormous)));
    assert!(!compare(&tagged(
        "Numeric",
        &format!("{{\"scale\":{enormous}}}")
    )));
    assert!(compare(&tagged(
        "Numeric",
        &format!("{{\"ignored\":{enormous}}}")
    )));
}

#[test]
fn stored_type_input_preserves_recursion_ignored_values_and_truncation_rules() {
    for depth in [62, 63, 64, 65] {
        let prefix = r#"{"Domain":{"schema":"s","name":"n","oid":1,"base":"#;
        let text = format!("{}\"Text\"{}", prefix.repeat(depth), "}}".repeat(depth));
        compare(&text);
    }
    for depth in [15, 16, 17, 125, 126, 127, 128, 129] {
        let text = format!(
            "{}\"Integer\"{}",
            "{\"Array\":".repeat(depth),
            "}".repeat(depth)
        );
        compare(&text);
        let ignored = format!("{}0{}", "[".repeat(depth * 2), "]".repeat(depth * 2));
        assert!(compare(&format!(
            "{{\"Numeric\":{{\"ignored\":{ignored}}}}}"
        )));
    }
    for &(tag, payload, _) in STRUCTS {
        let text = tagged(tag, payload);
        for end in 0..text.len() {
            if text.is_char_boundary(end) {
                assert!(!compare(&text[..end]));
            }
        }
        assert!(compare(&format!(" \r\n\t{text}\n ")));
        assert!(!compare(&format!("{text} null")));
        assert!(!compare(&format!("{text},")));
    }
}

#[test]
fn stored_type_input_matches_original_errors_for_mutated_declarations() {
    for &(tag, payload, _) in STRUCTS {
        let text = tagged(tag, payload);
        for position in 0..text.len() {
            for replacement in [b'0', b'"', b'\\', b'{', b']', b',', b':'] {
                let mut bytes = text.as_bytes().to_vec();
                bytes[position] = replacement;
                compare(std::str::from_utf8(&bytes).unwrap());
            }
        }
    }
}
