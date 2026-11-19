//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::{complete, SIMPLE};
use super::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(untagged)]
enum Content<T> {
    Value(T),
}

type Read<T> = fn(
    &str,
    usize,
    &MemoryBudget,
    &CancellationToken,
    &CancellationToken,
) -> Result<Produced<T>, SQLError>;

fn compare<T: DeserializeOwned + Serialize + std::fmt::Debug>(text: &str, read: Read<T>) -> bool {
    let expected = serde_json::from_str::<Content<T>>(text);
    let memory = MemoryBudget::new(1 << 25);
    let token = CancellationToken::new();
    let actual = read(text, 128, &memory, &token, &token);
    assert_eq!(actual.is_ok(), expected.is_ok(), "{text}: {actual:?}");
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(Content::Value(expected))) = (actual, expected) {
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
        assert_eq!(actual.reserved_bytes(), memory.used());
    }
    assert_eq!(memory.used(), 0);
    valid
}

#[test]
fn stored_constraint_content_preserves_original_fields_and_untyped_numbers() {
    assert!(compare(
        &complete().to_string(),
        read_stored_columns_content_json
    ));
    for number in [
        "-0",
        "0.0",
        "0e0",
        "32768",
        "18446744073709551616",
        "1e9999",
    ] {
        for name in [
            "attribute_number",
            "collation_oid",
            "missing_value",
            "unknown",
        ] {
            compare(
                &SIMPLE.replace("\"name\":", &format!("\"{name}\":{number},\"name\":")),
                read_stored_columns_content_json,
            );
        }
        compare(
            &format!(r#"{{"relation":{number}}}"#),
            read_stored_relation_oids_content_json,
        );
    }
    for text in [
        r#"[{"expr":{"Literal":true}}]"#,
        r#"[{"name":null,"expr":{"Not":{"Literal":false}},"catalog_oid":-0}]"#,
        r#"[[null,null,null,true,{"Literal":true}]]"#,
        r#"[{"expr":{"Literal":true},"expr":{"Literal":false}}]"#,
        r#"[{"expr":{"Literal":true},"unknown":"\ud800"}]"#,
    ] {
        compare(text, read_stored_checks_content_json);
    }
    for text in [
        r#"{"relation":40000,"row_type":40001,"array_type":40002}"#,
        "[40000]",
        r#"{"relation":40000,"unknown":true}"#,
        r#"{"relation":40000,"relation":40001}"#,
    ] {
        compare(text, read_stored_relation_oids_content_json);
    }
    let attributes = r#"[{"number":-0,"type_length":-1,"type_modifier":-1,"dimensions":0,"by_value":false,"alignment":"i","storage":"x","collation":0,"identity":"","is_local":true,"inheritance_count":0}]"#;
    assert!(compare(
        attributes,
        read_stored_dropped_attributes_content_json
    ));
    for extra in [r#""number":1,"#, r#""unknown":true,"#] {
        compare(
            &attributes.replacen('{', &format!("{{{extra}"), 1),
            read_stored_dropped_attributes_content_json,
        );
    }
}

#[test]
fn stored_constraint_content_keeps_generic_depth_and_strict_discarded_strings() {
    for depth in [0, 1, 63, 123, 124, 125, 126, 127, 128] {
        let value = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        for name in ["unknown", "missing_value"] {
            compare(
                &SIMPLE.replace("\"name\":", &format!("\"{name}\":{value},\"name\":")),
                read_stored_columns_content_json,
            );
        }
    }
    for unknown in [r#""\ud800""#, r#"{"x":"\ud800"}"#, "1e9999"] {
        compare(
            &SIMPLE.replace("\"name\":", &format!("\"unknown\":{unknown},\"name\":")),
            read_stored_columns_content_json,
        );
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for depth in [0, 1, 2, 129, usize::MAX] {
        assert!(read_stored_columns_content_json(SIMPLE, depth, &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    assert!(read_stored_columns_content_json(SIMPLE, 3, &memory, &token, &token).is_ok());
}

#[test]
fn stored_constraint_content_matches_original_truncations_and_mutations() {
    for text in [
        SIMPLE,
        r#"[["x","Integer",null,null,-0,[1,-0,1.5],false,false]]"#,
    ] {
        for boundary in 0..text.len() {
            compare(&text[..boundary], read_stored_columns_content_json);
        }
        for (index, byte) in text.bytes().enumerate() {
            for replacement in [b'"', b':', b',', b'{', b']', b'0', b'x'] {
                if byte == replacement {
                    continue;
                }
                let mut changed = text.as_bytes().to_vec();
                changed[index] = replacement;
                compare(
                    std::str::from_utf8(&changed).unwrap(),
                    read_stored_columns_content_json,
                );
            }
        }
    }
}

#[test]
fn stored_constraint_content_retains_native_payload_and_retires_failed_owners() {
    let text = complete()
        .to_string()
        .replace("한글_column", &"x".repeat(65536));
    let memory = MemoryBudget::new(1 << 25);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output =
            Some(read_stored_columns_content_json(&text, 128, &memory, &token, &token).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocations.bytes_max).unwrap() <= memory.peak());
    let peak = memory.peak();
    token.cancel();
    assert_eq!(output.as_ref().unwrap()[0].name.len(), 65536);
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(997).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(read_stored_columns_content_json(&text, 128, &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 25);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let cancel = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let result = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            cancel,
            65536,
            || read_stored_columns_content_json(&text, 128, &memory, &original, &invoking),
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
