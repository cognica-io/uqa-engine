//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_foreign_input_preserves_untagged_numeric_duplicate_and_positional_forms() {
    let mut row = row();
    for version in [
        "-0", "0", "1", "2", "3", "4", "255", "256", "1.0", "1e0", "1e9999", "null",
    ] {
        for suffix in [
            "",
            r#", "unknown":{"value":1e9999}"#,
            r#", "unknown":"\ud800""#,
            r#", "columns":[]"#,
            r#", "option_order":["a","a"]"#,
            r#", "catalog_oids":{"relation":-0}"#,
        ] {
            row.columns_json = format!(r#"{{"version":{version},"columns":[]{suffix}}}"#);
            for format in [None, Some(1), Some(2)] {
                compare(&row, format);
            }
        }
    }
    for text in [
        "null",
        "false",
        "{}",
        "[]",
        "[null]",
        "[1]",
        "[1,[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],null,null,null,null,[]]",
        "[1,[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],null,null,null,null,[],[],[]]",
        r#"[["x","Integer",-0,null,-0,[-0,1.5],false,false]]"#,
        r#"{"version":1,"columns":[{"name":"x","ty":"Integer","primary_key":false,"not_null":false,"collation_oid":-0,"attribute_number":-0}]}"#,
        r#"{"version":1,"columns":[],"ver\u0073ion":1}"#,
    ] {
        row.columns_json = text.into();
        compare(&row, None);
    }
    let full: serde_json::Value = serde_json::from_str(&super::row().columns_json).unwrap();
    let sequence: Vec<_> = [
        "version",
        "object_id",
        "catalog_oids",
        "row_type_array_name",
        "server_reference",
        "option_order",
        "columns",
        "dropped_attributes",
        "checks",
    ]
    .into_iter()
    .map(|name| full[name].clone())
    .collect();
    for length in 0..=sequence.len() {
        row.columns_json = serde_json::to_string(&sequence[..length]).unwrap();
        compare(&row, Some(2));
    }
}

#[test]
fn stored_foreign_input_preserves_complete_error_precedence_and_generic_depth() {
    let original = row();
    for options in ["null", "[]", r#"{"z":false}"#, r#"{"z":"\ud800"}"#, "{}"] {
        let mut row = original.clone();
        row.options_json = options.into();
        for schema in [
            "{",
            r#"{"version":99,"columns":[]}"#,
            r#"{"version":3,"columns":[],"option_order":[]}"#,
        ] {
            row.columns_json = schema.into();
            compare(&row, Some(2));
        }
    }
    for depth in [0, 1, 63, 122, 123, 124, 125, 126, 127, 128] {
        let value = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        let mut row = original.clone();
        row.columns_json = format!(r#"{{"version":1,"columns":[],"unknown":{value}}}"#);
        compare(&row, None);
        row.columns_json = format!(
            r#"{{"version":1,"columns":[{{"name":"x","ty":"Text","primary_key":false,"not_null":false,"missing_value":{value}}}]}}"#
        );
        compare(&row, None);
    }
}

#[test]
fn stored_foreign_input_matches_original_truncation_and_mutation_rejection() {
    let mut row = row();
    for text in [
        r#"{"version":1,"columns":[]}"#,
        r#"[{"name":"x","ty":"Integer","primary_key":false,"not_null":false}]"#,
    ] {
        for boundary in 0..text.len() {
            row.columns_json = text[..boundary].into();
            compare(&row, None);
        }
        for (index, byte) in text.bytes().enumerate() {
            for replacement in [b'"', b':', b',', b'{', b']', b'0', b'x'] {
                if byte == replacement {
                    continue;
                }
                let mut changed = text.as_bytes().to_vec();
                changed[index] = replacement;
                row.columns_json = String::from_utf8(changed).unwrap();
                compare(&row, None);
            }
        }
    }
}
