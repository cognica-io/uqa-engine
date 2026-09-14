//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::kuromoji::tokenizer::tests::{model, raw_analysis};
use crate::kuromoji::{
    JapaneseAnalyzer, JapaneseFilter, JapaneseTokenizer, KuromojiDictionary, KuromojiLimits,
    KuromojiOptions, KuromojiOrigin, KuromojiOutput, KuromojiToken, UserDictionary,
    UserDictionaryLimits,
};
use crate::{AnalysisResult, AnalyzedText, FilteredText};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[test]
fn japanese_filters_and_default_analyzer_match_complete_docker_outputs() {
    verify(
        include_bytes!("../../../../../../tests/parity/kuromoji/filter_cases.json"),
        include_bytes!("../../../../../../tests/parity/kuromoji/filter_expected.jsonl"),
        include_str!("../../../../../../tests/parity/kuromoji/filter_manifest.json"),
    );
}

#[test]
fn japanese_number_prefixes_and_composition_match_complete_docker_outputs() {
    verify(
        include_bytes!("../../../../../../tests/parity/kuromoji/number_cases.json"),
        include_bytes!("../../../../../../tests/parity/kuromoji/number_expected.jsonl"),
        include_str!("../../../../../../tests/parity/kuromoji/number_manifest.json"),
    );
}

#[test]
fn japanese_completion_components_match_complete_docker_outputs() {
    verify(
        include_bytes!("../../../../../../tests/parity/kuromoji/completion_cases.json"),
        include_bytes!("../../../../../../tests/parity/kuromoji/completion_expected.jsonl"),
        include_str!("../../../../../../tests/parity/kuromoji/completion_manifest.json"),
    );
}

fn verify(cases_bytes: &[u8], expected_bytes: &[u8], manifest: &str) {
    let manifest: Value = serde_json::from_str(manifest).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(cases_bytes)),
        manifest["cases_sha256"]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(expected_bytes)),
        manifest["expected_sha256"]
    );
    let cases: Vec<Value> = serde_json::from_slice(cases_bytes).unwrap();
    let expected: Vec<Value> = std::str::from_utf8(expected_bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(cases.len(), expected.len());
    assert_eq!(json!(cases.len()), manifest["fixture_count"]);
    let model = model();
    for (case, expected) in cases.iter().zip(expected) {
        assert_eq!(case["id"], expected["id"]);
        let id = case["id"].as_str().unwrap();
        let result = run_case(case, &model);
        if expected.get("error").is_some() {
            assert!(result.is_err(), "{id}: {result:?}");
            continue;
        }
        let mut result = result.unwrap_or_else(|error| panic!("{id}: {error}"));
        if case["kind"] == "completion_romanize" || case["kind"] == "completion_units" {
            assert_eq!(
                json!(result.as_array().unwrap().len()),
                expected["result_count"],
                "{id}"
            );
            if let Some(results) = expected.get("results") {
                assert_eq!(&result, results, "{id}");
            }
            assert_eq!(
                format!("{:x}", Sha256::digest(serde_json::to_vec(&result).unwrap())),
                expected["sha256"],
                "{id}"
            );
            continue;
        }
        if case["kind"] == "normalize" || case["kind"] == "completion_normalize" {
            assert_eq!(result, expected["normalized_utf16"], "{id}");
            continue;
        }
        if case["kind"] == "number_normalize" || case["kind"] == "number_units" {
            let count_field = if case["kind"] == "number_normalize" {
                "normalized_unit_count"
            } else {
                "normalization_count"
            };
            assert_eq!(
                json!(result.as_array().unwrap().len()),
                expected[count_field],
                "{id}"
            );
            if let Some(units) = expected.get("normalized_utf16") {
                assert_eq!(&result, units, "{id}");
            }
            assert_eq!(
                format!("{:x}", Sha256::digest(serde_json::to_vec(&result).unwrap())),
                expected["sha256"],
                "{id}"
            );
            continue;
        }
        result.sort_all_objects();
        assert_eq!(
            json!(result["tokens"].as_array().unwrap().len()),
            expected["token_count"],
            "{id}"
        );
        if let Some(analysis) = expected.get("analysis") {
            assert_eq!(&result, analysis, "{id}");
        }
        assert_eq!(
            format!("{:x}", Sha256::digest(serde_json::to_vec(&result).unwrap())),
            expected["sha256"],
            "{id}"
        );
    }
}

fn run_case(case: &Value, model: &Arc<KuromojiDictionary>) -> AnalysisResult<Value> {
    let input = input(case);
    if case["kind"] == "completion_romanize" || case["kind"] == "completion_units" {
        return completion(case, &input, model);
    }
    let normalize = |units: &[u16]| {
        crate::kuromoji::normalize_number_utf16(units, KuromojiLimits::default(), &mut || Ok(()))
    };
    if case["kind"] == "number_normalize" {
        return Ok(json!(normalize(&input)?));
    }
    if case["kind"] == "number_units" {
        let mut normalized = Vec::with_capacity(input.len() * 2);
        for unit in input {
            normalized.push(normalize(&[unit])?);
            normalized.push(normalize(&[u16::from(b'1'), unit, u16::from(b'2')])?);
        }
        return Ok(json!(normalized));
    }
    let user = case["user_dictionary"]
        .as_str()
        .map(|source| UserDictionary::compile(source, model, UserDictionaryLimits::default()))
        .transpose()?
        .flatten();
    if case["kind"] == "completion_analyzer" || case["kind"] == "completion_normalize" {
        return completion_analysis(case, &input, model, user);
    }
    let options = KuromojiOptions {
        mode: serde_json::from_value(case.get("mode").cloned().unwrap_or(json!("search"))).unwrap(),
        discard_punctuation: case["discard_punctuation"].as_bool().unwrap_or(true),
        discard_compound_token: case["discard_compound_token"].as_bool().unwrap_or(true),
        n_best_cost: case["n_best_cost"].as_i64().unwrap_or(0) as i32,
    };
    if case["kind"] == "analyzer" || case["kind"] == "normalize" {
        let analyzer = JapaneseAnalyzer::new(model.clone(), user, options.mode)?;
        let text = String::from_utf16(&input).unwrap();
        return if case["kind"] == "normalize" {
            Ok(json!(analyzer
                .normalize(&text)?
                .encode_utf16()
                .collect::<Vec<_>>()))
        } else {
            Ok(common_raw(&analyzer.analyze(&text)?))
        };
    }
    let filters: Vec<JapaneseFilter> = serde_json::from_value(case["filters"].clone()).unwrap();
    if case["kind"] == "pipeline" {
        let source = String::from_utf16(&input).unwrap();
        let analyzer = JapaneseAnalyzer::with_filters(model.clone(), user, options, &filters)?;
        return Ok(common_raw(&analyzer.analyze(&source)?));
    }
    let initial = if case["kind"] == "tokens" {
        let tokens = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(token)
            .collect();
        KuromojiOutput::from_tokens(
            tokens,
            case["final_offset_utf16"]
                .as_u64()
                .map_or(input.len(), |value| value as usize),
            case["final_position_increment"].as_u64().unwrap_or(0) as u32,
        )
    } else {
        JapaneseTokenizer::new(model.clone(), user, options)?.tokenize_utf16(
            &input,
            KuromojiLimits::default(),
            &mut || Ok(()),
        )?
    };
    let mut native = initial.clone();
    for filter in &filters {
        native = filter.apply(native, model)?;
    }
    let expected = raw_analysis(&native);
    if let Ok(source) = String::from_utf16(&input) {
        let mut common = initial.into_analyzed(&FilteredText::new(&source))?;
        for filter in &filters {
            common = filter.filter_analyzed(common, model)?;
        }
        assert_eq!(
            common_raw(&common),
            expected,
            "{} common bridge",
            case["id"]
        );
    }
    Ok(expected)
}
fn input(case: &Value) -> Vec<u16> {
    if let Some(range) = case.get("input_utf16_range") {
        (range[0].as_u64().unwrap()..range[1].as_u64().unwrap())
            .map(|unit| u16::try_from(unit).unwrap())
            .collect()
    } else if let Some(raw) = case.get("input_utf16") {
        serde_json::from_value(raw.clone()).unwrap()
    } else if let Some(parts) = case["input_parts"].as_array() {
        parts
            .iter()
            .flat_map(|part| {
                part["text"]
                    .as_str()
                    .unwrap()
                    .repeat(part["repeat"].as_u64().unwrap_or(1) as usize)
                    .encode_utf16()
                    .collect::<Vec<_>>()
            })
            .collect()
    } else {
        case["input"]
            .as_str()
            .unwrap()
            .repeat(case["repeat"].as_u64().unwrap_or(1) as usize)
            .encode_utf16()
            .collect()
    }
}
fn optional(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
fn term_units(value: &Value) -> Vec<u16> {
    let range = |field: &str| {
        let range = &value[field];
        (range[0].as_u64().unwrap()..range[1].as_u64().unwrap())
            .map(|unit| u16::try_from(unit).unwrap())
    };
    if value.get("term_utf16_pairs").is_some() {
        let suffix: Vec<u16> =
            serde_json::from_value(value.get("term_suffix_utf16").cloned().unwrap_or(json!([])))
                .unwrap();
        let mut units = Vec::new();
        for first in range("term_utf16_pairs") {
            for second in range("term_utf16_pairs") {
                units.extend([first, second]);
                units.extend_from_slice(&suffix);
                units.push(0);
            }
        }
        units
    } else if value.get("term_utf16_range").is_some() {
        let separator: Vec<u16> = serde_json::from_value(
            value
                .get("term_separator_utf16")
                .cloned()
                .unwrap_or(json!([])),
        )
        .unwrap();
        range("term_utf16_range")
            .flat_map(|unit| std::iter::once(unit).chain(separator.iter().copied()))
            .collect()
    } else {
        value.get("term_utf16").map_or_else(
            || value["term"].as_str().unwrap().encode_utf16().collect(),
            |raw| serde_json::from_value(raw.clone()).unwrap(),
        )
    }
}
fn token(value: &Value) -> KuromojiToken {
    KuromojiToken {
        errors: crate::kuromoji::attributes::AttributeErrors::default(),
        term_utf16: term_units(value),
        start_utf16: value["start_utf16"].as_u64().unwrap() as usize,
        end_utf16: value["end_utf16"].as_u64().unwrap() as usize,
        position_increment: value["position_increment"].as_u64().unwrap_or(1) as u32,
        position_length: value["position_length"].as_u64().unwrap_or(1) as u32,
        keyword: value["keyword"].as_bool().unwrap_or(false),
        origin: Some(KuromojiOrigin::Unknown),
        part_of_speech: optional(&value["part_of_speech"]),
        base_form: optional(&value["base_form"]),
        reading: optional(&value["reading"]),
        pronunciation: optional(&value["pronunciation"]),
        inflection_type: optional(&value["inflection_type"]),
        inflection_form: optional(&value["inflection_form"]),
    }
}
pub(super) fn common_raw(output: &AnalyzedText) -> Value {
    let units = |text: Option<&str>| text.map(|text| text.encode_utf16().collect::<Vec<_>>());
    let tokens: Vec<_> = output.tokens().iter().map(|token| {
        let fields = token.japanese_morphology().map_or([None;6], |value| value.fields().map(|field| field.map(String::as_str)));
        let offsets = token.offsets().unwrap();
        json!({"term_utf16":token.term().utf16(),"start_utf16":offsets.utf16.start,"end_utf16":offsets.utf16.end,
            "position_increment":token.position_increment(),"position_length":token.position_length(),"keyword":token.is_keyword(),
            "part_of_speech_utf16":units(fields[0]),"base_form_utf16":units(fields[1]),"reading_utf16":units(fields[2]),"pronunciation_utf16":units(fields[3]),"inflection_type_utf16":units(fields[4]),"inflection_form_utf16":units(fields[5])})
    }).collect();
    json!({"tokens":tokens,"final_offset_utf16":output.final_offsets().utf16.end,"final_position_increment":output.final_position_increment()})
}

fn completion(case: &Value, input: &[u16], model: &KuromojiDictionary) -> AnalysisResult<Value> {
    let romanize = |units: &[u16]| {
        crate::kuromoji::romanize_completion_utf16(
            units,
            model,
            KuromojiLimits::default(),
            &mut || Ok(()),
        )
    };
    if case["kind"] == "completion_romanize" {
        return Ok(json!(romanize(input)?));
    }
    let mut results = Vec::new();
    for &unit in input {
        results.push(romanize(&[unit])?);
        results.push(romanize(&[0x30b7, unit, 0x30ab])?);
    }
    Ok(json!(results))
}

fn completion_analysis(
    case: &Value,
    input: &[u16],
    model: &Arc<KuromojiDictionary>,
    user: Option<Arc<UserDictionary>>,
) -> AnalysisResult<Value> {
    let mode = serde_json::from_value(
        case.get("completion_mode")
            .cloned()
            .unwrap_or(json!("index")),
    )
    .unwrap();
    let analyzer = JapaneseAnalyzer::completion(model.clone(), user, mode)?;
    let text = String::from_utf16(input).unwrap();
    if case["kind"] == "completion_normalize" {
        Ok(json!(analyzer
            .normalize(&text)?
            .encode_utf16()
            .collect::<Vec<_>>()))
    } else {
        Ok(common_raw(&analyzer.analyze(&text)?))
    }
}
