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
    let cases_bytes = include_bytes!("../../../../../../tests/parity/kuromoji/filter_cases.json");
    let expected_bytes =
        include_bytes!("../../../../../../tests/parity/kuromoji/filter_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/kuromoji/filter_manifest.json"
    ))
    .unwrap();
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
        if case["kind"] == "normalize" {
            assert_eq!(result, expected["normalized_utf16"], "{id}");
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
    let user = case["user_dictionary"]
        .as_str()
        .map(|source| UserDictionary::compile(source, model, UserDictionaryLimits::default()))
        .transpose()?
        .flatten();
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
    if let Some(raw) = case.get("input_utf16") {
        serde_json::from_value(raw.clone()).unwrap()
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
fn token(value: &Value) -> KuromojiToken {
    KuromojiToken {
        errors: crate::kuromoji::attributes::AttributeErrors::default(),
        term_utf16: value.get("term_utf16").map_or_else(
            || value["term"].as_str().unwrap().encode_utf16().collect(),
            |raw| serde_json::from_value(raw.clone()).unwrap(),
        ),
        start_utf16: value["start_utf16"].as_u64().unwrap() as usize,
        end_utf16: value["end_utf16"].as_u64().unwrap() as usize,
        position_increment: value["position_increment"].as_u64().unwrap_or(1) as u32,
        position_length: value["position_length"].as_u64().unwrap_or(1) as u32,
        keyword: value["keyword"].as_bool().unwrap_or(false),
        origin: KuromojiOrigin::Unknown,
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
