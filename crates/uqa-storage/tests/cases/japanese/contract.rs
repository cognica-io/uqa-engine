//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared provider contract projects existing pinned Java observations into complete occurrences.

use std::{collections::BTreeMap, sync::Arc};

use serde_json::{json, Value};
use uqa_analysis::{Analyzer, AnalyzerResources, CompiledAnalyzer, TokenLengthPolicy, TokenTerm};
use uqa_core::{TokenOccurrence, TokenOffsets};
use uqa_storage::{
    inverted_index::{AnalyzedField, IndexedFieldMetadata},
    AnalyzerPhase, InvertedIndex, TokenTermKey,
};

pub struct JapaneseCase {
    pub id: String,
    pub input: String,
    pub revision: Arc<CompiledAnalyzer>,
    pub expected: AnalyzedField,
}

pub fn cases() -> Vec<JapaneseCase> {
    let Ok(ordinary) = uqa_analysis::get_analyzer("kuromoji") else {
        let error = serde_json::from_value::<Analyzer>(json!({
            "tokenizer": {"type": "kuromoji_tokenizer"},
        }))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unknown variant `kuromoji_tokenizer`"));
        return Vec::new();
    };
    let sources = [
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/filter_cases.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/filter_expected.jsonl"
            )),
            &["analyzer_search_1", "analyzer_search_5"][..],
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/nbest_cases.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/nbest_expected.jsonl"
            )),
            &[
                "compound_search_1000",
                "homographs_search",
                "user_segments_search",
                "unknown_extended",
            ][..],
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/completion_cases.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/kuromoji/completion_expected.jsonl"
            )),
            &["analyzer-index-17", "analyzer-index-18"][..],
        ),
    ];
    let mut result = Vec::new();
    for (inputs, outputs, selected) in sources {
        let inputs: Vec<Value> = serde_json::from_str(inputs).unwrap();
        let outputs: Vec<Value> = outputs
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(inputs.len(), outputs.len());
        for id in selected {
            let position = inputs.iter().position(|case| case["id"] == *id).unwrap();
            let input = &inputs[position];
            let output = &outputs[position];
            assert_eq!(output["id"], *id);
            let config = match input["kind"].as_str() {
                Some("analyzer") => ordinary.clone(),
                Some("completion_analyzer") => {
                    uqa_analysis::get_analyzer("kuromoji_completion").unwrap()
                }
                None => {
                    let mut tokenizer = input.clone();
                    tokenizer.as_object_mut().unwrap().remove("id");
                    tokenizer.as_object_mut().unwrap().remove("input");
                    tokenizer["type"] = json!("kuromoji_tokenizer");
                    serde_json::from_value(json!({"tokenizer": tokenizer})).unwrap()
                }
                other => panic!("unexpected reference kind {other:?}"),
            };
            let text = input["input"].as_str().unwrap();
            result.push(JapaneseCase {
                id: (*id).into(),
                input: text.into(),
                revision: config.compile().unwrap(),
                expected: project(text, &output["analysis"]),
            });
        }
    }
    assert_eq!(result.len(), 8);
    result
}

fn offsets(input: &str, start: u64, end: u64) -> TokenOffsets {
    let mut utf16 = 0;
    let mut boundaries = BTreeMap::from([(0, 0)]);
    for (byte, ch) in input.char_indices() {
        utf16 += ch.len_utf16() as u64;
        boundaries.insert(utf16, (byte + ch.len_utf8()) as u64);
    }
    TokenOffsets {
        start_utf8: boundaries[&start],
        end_utf8: boundaries[&end],
        start_utf16: start,
        end_utf16: end,
    }
}

fn project(input: &str, observation: &Value) -> AnalyzedField {
    let mut field: AnalyzedField = AnalyzedField {
        length: 0,
        terms: BTreeMap::new(),
        final_offsets: offsets(
            input,
            observation["final_offset_utf16"].as_u64().unwrap(),
            observation["final_offset_utf16"].as_u64().unwrap(),
        ),
        final_position_increment: observation["final_position_increment"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap(),
    };
    let mut position = -1_i64;
    for token in observation["tokens"].as_array().unwrap() {
        let increment = token["position_increment"].as_i64().unwrap();
        position += increment;
        field.length += u64::from(increment > 0);
        let term = TokenTerm::from_utf16(
            token["term_utf16"]
                .as_array()
                .unwrap()
                .iter()
                .map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap())
                .collect(),
        );
        field
            .terms
            .entry(TokenTermKey::from_term(&term))
            .or_default()
            .push(TokenOccurrence {
                position: position.try_into().unwrap(),
                position_length: token["position_length"]
                    .as_u64()
                    .unwrap()
                    .try_into()
                    .unwrap(),
                offsets: Some(offsets(
                    input,
                    token["start_utf16"].as_u64().unwrap(),
                    token["end_utf16"].as_u64().unwrap(),
                )),
            });
    }
    field
}

pub fn populate(index: &mut dyn InvertedIndex, case: &JapaneseCase) {
    index
        .set_field_analyzer_revision("body", case.revision.clone(), AnalyzerPhase::Both)
        .unwrap();
    index
        .try_add_documents(
            [
                (7, case.input.as_str()),
                (65_536, case.input.as_str()),
                (11, ""),
            ]
            .map(|(id, text)| (id, BTreeMap::from([("body".into(), text.into())])))
            .to_vec(),
        )
        .unwrap();
}

pub fn restore(index: &mut dyn InvertedIndex, case: &JapaneseCase) {
    let owner = AnalyzerResources::default();
    let revision = owner
        .restore_json(case.revision.descriptor().canonical_json())
        .unwrap();
    let search = owner.compile(&uqa_analysis::keyword_analyzer()).unwrap();
    index
        .set_field_analyzer_revisions("body", revision, search)
        .unwrap();
}

pub fn verify(index: &dyn InvertedIndex, case: &JapaneseCase) {
    assert_eq!(
        case.revision.descriptor().length_policy(),
        TokenLengthPolicy::DiscountOverlaps
    );
    assert_eq!(index.doc_count().unwrap(), 3, "{}", case.id);
    assert_eq!(index.field_doc_count("body").unwrap(), 3);
    assert_eq!(
        index.total_field_length("body").unwrap(),
        2 * case.expected.length
    );
    assert_eq!(
        index.vocabulary_keys("body").unwrap(),
        case.expected.terms.keys().cloned().collect::<Vec<_>>()
    );
    for id in [7, 65_536] {
        assert_eq!(
            index.indexed_field_metadata(id, "body").unwrap().unwrap(),
            IndexedFieldMetadata::new(&case.revision, &case.expected),
            "{}",
            case.id
        );
        assert_eq!(
            index.get_doc_length(id, "body").unwrap(),
            case.expected.length
        );
    }
    assert_eq!(index.get_doc_length(11, "body").unwrap(), 0);
    for (term, occurrences) in &case.expected.terms {
        assert_eq!(index.doc_freq_key("body", term).unwrap(), 2);
        let postings = index.get_occurrence_postings("body", term).unwrap();
        assert_eq!(postings.len(), 2);
        let mut cursor = index.posting_cursor_key("body", term).unwrap();
        for (posting, id) in postings.iter().zip([7, 65_536]) {
            assert_eq!(posting.doc_id, id);
            assert_eq!(&posting.occurrences, occurrences, "{}", case.id);
            assert_eq!(
                index.get_occurrences(id, "body", term).unwrap(),
                *occurrences
            );
            assert_eq!(
                index.get_term_freq_key(id, "body", term).unwrap(),
                occurrences.len() as u64
            );
            let score = cursor.current().unwrap();
            assert_eq!(
                (score.doc_id, score.term_freq, score.doc_length),
                (id, occurrences.len() as u64, case.expected.length)
            );
            cursor.advance().unwrap();
        }
        assert!(cursor.current().is_none());
    }
}
