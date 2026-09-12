//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise generic attributes against the same complete Docker stream snapshots.

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::nori::filters::stream::FilterStream;
use crate::nori::{
    DictionaryLimits, KoreanFilter, KoreanTokenizer, NoriDictionary, NoriLimits, NoriOptions,
    NoriOrigin, UserDictionary, UserDictionaryLimits,
};

fn model() -> &'static Arc<NoriDictionary> {
    static MODEL: OnceLock<Arc<NoriDictionary>> = OnceLock::new();
    MODEL.get_or_init(|| {
        NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default()).unwrap()
    })
}

#[test]
fn budgeted_conversion_retains_hidden_terminal_attributes_and_releases_native_boxes() {
    use uqa_core::memory::MemoryBudget;
    let budget = MemoryBudget::new(1 << 20);
    let input = FilteredText::new("韓國");
    input.prepare_coordinates(&budget, &mut || Ok(())).unwrap();
    let tokenizer = KoreanTokenizer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let raw = tokenizer
        .tokenize_budgeted(input.as_str(), NoriLimits::default(), &budget, &mut || {
            Ok(())
        })
        .unwrap();
    let (mut raw, mut memory) = raw.into_parts();
    memory.grow(std::mem::size_of::<NoriToken>()).unwrap();
    raw.terminal = Some(Box::new(raw.tokens.pop().unwrap()));
    raw.final_position_increment = 1;
    let expected = raw.clone().into_analyzed(&input).unwrap();
    let actual =
        AnalyzedText::from_nori_budgeted(Budgeted::new(raw, memory), &input, &mut || Ok(()))
            .unwrap();
    assert_eq!(*actual, expected);
    assert!(actual.tokens().is_empty());
    let terminal = actual.batch.terminal.as_ref().unwrap();
    assert_eq!(terminal.term(), "韓國");
    assert_eq!(
        terminal.korean_morphology().unwrap().reading.as_deref(),
        Some("한국")
    );
    assert_eq!(
        actual.reserved_bytes(),
        std::mem::size_of::<AnalysisToken>() + "韓國".len() + "한국".len()
    );
    drop(expected);
    drop(input);
    drop(actual);
    assert_eq!(budget.used(), 0);
}

fn units(value: &Value) -> Vec<u16> {
    serde_json::from_value(value.clone()).unwrap()
}

fn attributes(value: &Value, input: &FilteredText<'_>) -> AnalysisToken {
    let range = value["start_utf16"].as_u64().unwrap() as usize
        ..value["end_utf16"].as_u64().unwrap() as usize;
    let morphology = (!value["pos_type"].is_null()).then(|| KoreanMorphology {
        pos_type: serde_json::from_value(value["pos_type"].clone()).unwrap(),
        left_pos: serde_json::from_value(value["left_pos"].clone()).unwrap(),
        right_pos: serde_json::from_value(value["right_pos"].clone()).unwrap(),
        reading: value["reading_utf16"]
            .as_array()
            .map(|_| String::from_utf16(&units(&value["reading_utf16"])).unwrap()),
        morphemes: value["morphemes"].as_array().map(|parts| {
            parts
                .iter()
                .map(|part| crate::nori::NoriMorpheme {
                    surface_utf16: units(&part["surface_utf16"]),
                    pos: serde_json::from_value(part["pos"].clone()).unwrap(),
                })
                .collect()
        }),
        origin: NoriOrigin::Known,
    });
    AnalysisToken {
        term: TokenTerm::from_utf16(units(&value["term_utf16"])),
        offsets: Some(input.source_covering_offsets_utf16(range.clone()).unwrap()),
        position_increment: serde_json::from_value(value["position_increment"].clone()).unwrap(),
        position_length: serde_json::from_value(value["position_length"].clone()).unwrap(),
        keyword: value["keyword"].as_bool().unwrap_or(false),
        filtered_utf16: Some(range),
        korean_morphology: morphology,
        verbatim: false,
    }
}

fn input_stream(
    case: &Value,
    cases: &[Value],
    expected: &[Value],
) -> AnalysisResult<FilterStream<AnalysisToken>> {
    let pipeline = case["pipeline"].as_str().unwrap();
    let text = if pipeline == "synthetic" {
        let end = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|token| token["end_utf16"].as_u64().unwrap())
            .max()
            .unwrap_or(0)
            .max(case["final_offset_utf16"].as_u64().unwrap());
        // Synthetic sources control attributes and end state independently of source text.
        ".".repeat(usize::try_from(end).unwrap())
    } else {
        case["input"].as_str().unwrap().to_owned()
    };
    let input = FilteredText::new(&text);
    let (tokens, final_offset_utf16, final_position_increment) = match pipeline {
        "synthetic" => (
            case["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|token| attributes(token, &input))
                .collect(),
            case["final_offset_utf16"].as_u64().unwrap() as usize,
            serde_json::from_value(case["final_position_increment"].clone()).unwrap(),
        ),
        "whitespace" | "keyword" => {
            // These recorded source attributes isolate filters from tokenizer differences.
            let baseline = cases
                .iter()
                .position(|item| {
                    item["pipeline"] == case["pipeline"]
                        && item["input"] == case["input"]
                        && item["filters"].as_array().unwrap().is_empty()
                })
                .unwrap();
            let source = &expected[baseline]["analysis"];
            (
                source["tokens"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|token| attributes(token, &input))
                    .collect(),
                source["final_offset_utf16"].as_u64().unwrap() as usize,
                serde_json::from_value(source["final_position_increment"].clone()).unwrap(),
            )
        }
        _ => {
            let options = NoriOptions {
                decompound_mode: serde_json::from_value(
                    case.get("decompound_mode")
                        .cloned()
                        .unwrap_or(json!("none")),
                )
                .unwrap(),
                output_unknown_unigrams: case["output_unknown_unigrams"].as_bool().unwrap_or(false),
                discard_punctuation: case["discard_punctuation"].as_bool().unwrap_or(false),
            };
            let user = case["user_dictionary"]
                .as_str()
                .map(|source| {
                    UserDictionary::compile(source, model(), UserDictionaryLimits::default())
                })
                .transpose()?
                .flatten();
            let raw = KoreanTokenizer::new(model().clone(), user, options)?.tokenize(&text)?;
            (
                raw.tokens
                    .into_iter()
                    .map(|token| AnalysisToken::from_nori(token, &input))
                    .collect::<AnalysisResult<_>>()?,
                raw.final_offset_utf16,
                raw.final_position_increment,
            )
        }
    };
    Ok(FilterStream {
        tokens,
        terminal: None,
        final_offset_utf16,
        final_position_increment,
        context: input.projection(),
    })
}

fn snapshot(stream: &FilterStream<AnalysisToken>, keyword: bool) -> Value {
    let tokens: Vec<_> = stream.tokens.iter().map(|token| {
        let morphology = token.korean_morphology();
        let mut value = json!({
            "term_utf16": token.term().utf16(),
            "start_utf16": token.offsets().unwrap().utf16.start,
            "end_utf16": token.offsets().unwrap().utf16.end,
            "position_increment": token.position_increment(), "position_length": token.position_length(),
            "pos_type": morphology.map(|value| value.pos_type), "left_pos": morphology.map(|value| value.left_pos),
            "right_pos": morphology.map(|value| value.right_pos),
            "reading_utf16": morphology.and_then(|value| value.reading.as_ref()).map(|text| text.encode_utf16().collect::<Vec<_>>()),
            "morphemes": morphology.and_then(|value| value.morphemes.as_ref()),
        });
        if keyword { value["keyword"] = json!(token.is_keyword()); }
        value
    }).collect();
    json!({"tokens": tokens, "final_offset_utf16": stream.final_offset_utf16, "final_position_increment": stream.final_position_increment})
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

#[test]
fn generic_filter_kernel_matches_every_recorded_number_and_analysis_stream() {
    let mut checked = 0;
    for (case_bytes, expected_bytes, manifest, keyword) in [
        (
            include_bytes!("../../../../../tests/parity/nori/number_cases.json").as_slice(),
            include_bytes!("../../../../../tests/parity/nori/number_expected.jsonl").as_slice(),
            include_str!("../../../../../tests/parity/nori/number_manifest.json"),
            true,
        ),
        (
            include_bytes!("../../../../../tests/parity/nori/generic_cases.json").as_slice(),
            include_bytes!("../../../../../tests/parity/nori/generic_expected.jsonl").as_slice(),
            include_str!("../../../../../tests/parity/nori/generic_manifest.json"),
            true,
        ),
        (
            include_bytes!("../../../../../tests/parity/nori/analysis_cases.json").as_slice(),
            include_bytes!("../../../../../tests/parity/nori/analysis_expected.jsonl").as_slice(),
            include_str!("../../../../../tests/parity/nori/analysis_manifest.json"),
            false,
        ),
    ] {
        let manifest: Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(
            manifest["cases_sha256"],
            format!("{:x}", Sha256::digest(case_bytes))
        );
        assert_eq!(
            manifest["expected_sha256"],
            format!("{:x}", Sha256::digest(expected_bytes))
        );
        let cases: Vec<Value> = serde_json::from_slice(case_bytes).unwrap();
        let expected: Vec<Value> = std::str::from_utf8(expected_bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(cases.len(), expected.len());
        assert_eq!(json!(cases.len()), manifest["fixture_count"]);
        for (case, expected_row) in cases.iter().zip(&expected) {
            assert_eq!(case["id"], expected_row["id"]);
            if matches!(
                case["pipeline"].as_str().unwrap(),
                "normalize" | "unicode_lowercase"
            ) {
                continue;
            }
            let filters: Vec<KoreanFilter> = case
                .get("filters")
                .filter(|_| case["pipeline"] != "analyzer")
                .map_or_else(
                    || {
                        vec![
                            KoreanFilter::PartOfSpeech { stop_tags: None },
                            KoreanFilter::ReadingForm,
                            KoreanFilter::SimpleLowercase,
                        ]
                    },
                    |value| serde_json::from_value(value.clone()).unwrap(),
                );
            let result = input_stream(case, &cases, &expected).and_then(|stream| {
                filters.into_iter().try_fold(stream, |stream, filter| {
                    filter.compile().apply_stream(
                        stream,
                        Some(model()),
                        NoriLimits::default(),
                        &mut || Ok(()),
                    )
                })
            });
            if expected_row.get("error").is_some() {
                assert!(result.is_err(), "{}", case["id"]);
                continue;
            }
            let result = result.unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
            let value = snapshot(&result, keyword);
            if let Some(expected) = expected_row.get("analysis") {
                assert_eq!(&value, expected, "{}", case["id"]);
            }
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&canonical(&value)).unwrap())
                ),
                expected_row["sha256"],
                "{}",
                case["id"]
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 1023);
}
