//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One decoded reference model shared by the Nori integration tests.

use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uqa_analysis::nori::{DictionaryLimits, NoriDictionary};

pub(super) fn model() -> &'static Arc<NoriDictionary> {
    static MODEL: OnceLock<Arc<NoriDictionary>> = OnceLock::new();
    MODEL.get_or_init(|| {
        NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default()).unwrap()
    })
}

pub(super) fn raw_analysis(output: &uqa_analysis::nori::NoriOutput) -> Value {
    let tokens: Vec<_> = output.tokens.iter().map(|token| json!({
        "term_utf16": token.term_utf16, "start_utf16": token.start_utf16, "end_utf16": token.end_utf16,
        "position_increment": token.position_increment, "position_length": token.position_length,
        "pos_type": token.pos_type, "left_pos": token.left_pos, "right_pos": token.right_pos,
        "reading_utf16": token.reading.as_ref().map(|text| text.encode_utf16().collect::<Vec<_>>()), "morphemes": token.morphemes,
    })).collect();
    json!({"tokens": tokens, "final_offset_utf16": output.final_offset_utf16, "final_position_increment": output.final_position_increment})
}

pub(super) fn canonical(value: &Value, bytes: &mut Vec<u8>) {
    match value {
        Value::Object(object) => {
            bytes.push(b'{');
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key).unwrap();
                bytes.push(b':');
                canonical(&object[key], bytes);
            }
            bytes.push(b'}');
        }
        Value::Array(array) => {
            bytes.push(b'[');
            for (index, value) in array.iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                canonical(value, bytes);
            }
            bytes.push(b']');
        }
        _ => serde_json::to_writer(bytes, value).unwrap(),
    }
}

pub(super) fn string_tokens(output: &uqa_analysis::nori::NoriOutput) -> Value {
    let mut position = -1_i64;
    let projected: Vec<_> = output.tokens.iter().map(|token| {
            position += i64::from(token.position_increment);
            let parts = token.morphemes.as_ref().map(|parts| parts.iter().map(|part| json!({"surface": String::from_utf16(&part.surface_utf16).unwrap(), "pos": part.pos})).collect::<Vec<_>>());
            json!({"term": String::from_utf16(&token.term_utf16).unwrap(), "start_utf16": token.start_utf16, "end_utf16": token.end_utf16,
                "position": position, "position_increment": token.position_increment, "position_length": token.position_length,
                "pos_type": token.pos_type, "left_pos": token.left_pos, "right_pos": token.right_pos, "reading": token.reading, "morphemes": parts})
        }).collect();
    json!(projected)
}
