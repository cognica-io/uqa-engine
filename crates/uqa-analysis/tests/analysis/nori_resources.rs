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

pub(super) fn assert_generic_bridge(output: &uqa_analysis::nori::NoriOutput, input: &str) {
    let analyzed = output
        .clone()
        .into_analyzed(&uqa_analysis::FilteredText::new(input))
        .unwrap();
    assert_eq!(analyzed.final_offsets().utf8, input.len()..input.len());
    assert_eq!(
        analyzed.final_offsets().utf16,
        output.final_offset_utf16..output.final_offset_utf16
    );
    assert_eq!(
        analyzed.final_position_increment(),
        output.final_position_increment
    );
    assert_eq!(analyzed.tokens().len(), output.tokens.len());
    for (generic, raw) in analyzed.tokens().iter().zip(&output.tokens) {
        assert_eq!(generic.term().utf16().as_ref(), raw.term_utf16);
        assert_eq!(
            generic.filtered_utf16(),
            Some(&(raw.start_utf16..raw.end_utf16))
        );
        assert_eq!(
            generic.offsets().unwrap().utf16,
            raw.start_utf16..raw.end_utf16
        );
        assert!(input.get(generic.offsets().unwrap().utf8.clone()).is_some());
        assert_eq!(generic.position_increment(), raw.position_increment);
        assert_eq!(generic.position_length(), raw.position_length);
        assert_eq!(generic.is_keyword(), raw.keyword);
        let morphology = generic.korean_morphology().unwrap();
        assert_eq!(morphology.pos_type, raw.pos_type);
        assert_eq!(morphology.left_pos, raw.left_pos);
        assert_eq!(morphology.right_pos, raw.right_pos);
        assert_eq!(morphology.reading, raw.reading);
        assert_eq!(morphology.morphemes, raw.morphemes);
        assert_eq!(morphology.origin, raw.origin);
    }
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
