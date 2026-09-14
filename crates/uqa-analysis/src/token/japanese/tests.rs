//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::{json, Value};
use uqa_core::memory::MemoryError;

use super::*;
use crate::kuromoji::{KuromojiOptions, KuromojiOrigin, KuromojiResources};
use crate::{AnalysisError, AnalysisToken, CharFilter, TokenFilter};

fn tokenizer() -> JapaneseTokenizer {
    JapaneseTokenizer::new(
        KuromojiResources::default()
            .load_default()
            .unwrap()
            .model()
            .clone(),
        None,
        KuromojiOptions {
            discard_compound_token: false,
            ..KuromojiOptions::default()
        },
    )
    .unwrap()
}

fn attributes() -> KuromojiOutput {
    KuromojiOutput {
        tokens: vec![KuromojiToken {
            errors: crate::kuromoji::AttributeErrors::default(),
            term_utf16: vec![0xd83d],
            start_utf16: 0,
            end_utf16: 1,
            position_increment: 3,
            position_length: 2,
            keyword: true,
            part_of_speech: Some("名詞".into()),
            base_form: None,
            reading: Some(String::new()),
            pronunciation: Some("ヨミ".into()),
            inflection_type: Some("型".into()),
            inflection_form: Some("形".into()),
            origin: Some(KuromojiOrigin::User),
        }],
        terminal: None,
        final_offset_utf16: 3,
        final_position_increment: 4,
    }
}

#[test]
fn japanese_conversion_moves_attributes_and_retains_exact_units_and_wire_keys() {
    let raw = attributes();
    let term_pointer = raw.tokens[0].term_utf16.as_ptr();
    let pos_pointer = raw.tokens[0].part_of_speech.as_ref().unwrap().as_ptr();
    let output = raw.into_analyzed(&FilteredText::new("🙂a")).unwrap();
    let token = &output.tokens()[0];
    assert_eq!(token.term().utf16().as_ptr(), term_pointer);
    assert_eq!(
        token
            .japanese_morphology()
            .unwrap()
            .part_of_speech
            .as_ref()
            .unwrap()
            .as_ptr(),
        pos_pointer
    );
    assert_eq!(
        serde_json::to_value(&output).unwrap(),
        json!({
            "tokens": [{
                "term": {"utf16": [0xd83d]},
                "offsets": {"utf8": {"start": 0, "end": 4}, "utf16": {"start": 0, "end": 1}},
                "position_increment": 3, "position_length": 2, "keyword": true,
                "filtered_utf16": {"start": 0, "end": 1},
                "japanese_morphology": {
                    "part_of_speech": "名詞", "base_form": null, "reading": "",
                    "pronunciation": "ヨミ", "inflection_type": "型", "inflection_form": "形",
                    "origin": "user"
                }
            }],
            "final_offsets": {"utf8": {"start": 5, "end": 5}, "utf16": {"start": 3, "end": 3}},
            "final_position_increment": 4
        })
    );
    #[cfg(feature = "nori")]
    assert!(token.korean_morphology().is_none());
}

fn units(text: Option<&str>) -> Option<Vec<u16>> {
    text.map(|text| text.encode_utf16().collect())
}

#[test]
fn mapped_japanese_graph_and_all_attributes_match_the_pinned_compound_stream() {
    let expected: Value =
        include_str!("../../../../../tests/parity/kuromoji/tokenizer_expected.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|case| case["id"] == "compound_search_1_0")
            .unwrap();
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets("<b>関西国際空港に行きました。</b>")
        .unwrap();
    let output = tokenizer().tokenize_mapped(&filtered).unwrap();
    drop(filtered);
    for (actual, expected) in output
        .tokens()
        .iter()
        .zip(expected["analysis"]["tokens"].as_array().unwrap())
    {
        let morphology = actual.japanese_morphology().unwrap();
        let range = actual.filtered_utf16().unwrap();
        assert_eq!(
            json!({
                "term_utf16": actual.term().utf16(), "start_utf16": range.start - 1, "end_utf16": range.end - 1,
                "position_increment": actual.position_increment(), "position_length": actual.position_length(),
                "keyword": actual.is_keyword(),
                "part_of_speech_utf16": units(morphology.part_of_speech.as_deref()),
                "base_form_utf16": units(morphology.base_form.as_deref()),
                "reading_utf16": units(morphology.reading.as_deref()),
                "pronunciation_utf16": units(morphology.pronunciation.as_deref()),
                "inflection_type_utf16": units(morphology.inflection_type.as_deref()),
                "inflection_form_utf16": units(morphology.inflection_form.as_deref()),
            }),
            *expected
        );
        let start = expected["start_utf16"].as_u64().unwrap() as usize;
        let end = expected["end_utf16"].as_u64().unwrap() as usize;
        assert_eq!(actual.offsets().unwrap().utf16, start + 3..end + 3);
        assert_eq!(actual.offsets().unwrap().utf8, start * 3 + 3..end * 3 + 3);
    }
    assert_eq!(output.tokens().len(), 8);
    assert_eq!(output.final_offsets().utf16, 20..20);
    assert_eq!(output.final_position_increment(), 0);
}

#[test]
fn mapped_width_edits_and_rewritten_tokens_keep_covering_original_spans() {
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>ｶﾞ</b>", &budget, &mut || Ok(()))
        .unwrap();
    let filtered = CharFilter::CJKWidth
        .filter_mapped_budgeted(filtered, &budget, &mut || Ok(()))
        .unwrap();
    let original = tokenizer()
        .tokenize_mapped_budgeted(
            &filtered,
            KuromojiLimits::default(),
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
    drop(filtered);
    assert_eq!(original.tokens().len(), 1);
    let token = &original.tokens()[0];
    assert_eq!(token.term(), "ガ");
    assert_eq!(token.offsets().unwrap().utf16, 3..5);
    assert_eq!(token.offsets().unwrap().utf8, 3..9);
    let morphology = token.japanese_morphology().unwrap().clone();
    let source_before = budget.used() - original.reserved_bytes();
    let copy = original.clone_budgeted(&budget, || Ok(())).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &original.projection,
        &copy.projection
    ));
    drop(original);
    assert_eq!(budget.used(), source_before + copy.reserved_bytes());
    let expanded = TokenFilter::Synonym {
        synonyms: [("ガ".into(), vec!["GAS".into()])].into(),
        synonyms_path: None,
    }
    .filter_analyzed_budgeted(copy, || Ok(()))
    .unwrap();
    let expanded = TokenFilter::Lowercase
        .filter_analyzed_budgeted(expanded, || Ok(()))
        .unwrap();
    let expanded = TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 1,
        keep_short: false,
    }
    .filter_analyzed_budgeted(expanded, || Ok(()))
    .unwrap();
    assert_eq!(
        expanded
            .tokens()
            .iter()
            .map(|token| token.term().as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ガ", "g", "a", "s"]
    );
    for token in expanded.tokens() {
        assert_eq!(token.japanese_morphology(), Some(&morphology));
        assert_eq!(token.offsets().unwrap().utf16, 3..5);
        assert_eq!(token.offsets().unwrap().utf8, 3..9);
    }
    let empty = TokenFilter::Stop {
        language: String::new(),
        custom_words: vec!["ガ".into(), "g".into(), "a".into(), "s".into()],
    }
    .filter_analyzed_budgeted(expanded, || Ok(()))
    .unwrap();
    assert!(empty.tokens().is_empty());
    assert_eq!(
        empty.batch.terminal.as_ref().unwrap().japanese_morphology(),
        Some(&morphology)
    );
    assert_eq!(empty.final_offsets().utf16, 9..9);
    drop(empty);
    assert_eq!(budget.used(), 7);
    drop(other);
}

#[test]
fn mapped_failures_release_all_native_and_common_owners_without_mutating_borrowed_input() {
    let tokenizer = tokenizer();
    let input = CharFilter::CJKWidth
        .filter_with_offsets("ｶﾞ関西国際空港")
        .unwrap();
    let before = format!("{input:?}");
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let expected = tokenizer
        .tokenize_mapped_budgeted(&input, KuromojiLimits::default(), &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    for allowance in [0, 1, expected.reserved_bytes() - 1, baseline.peak() - 1] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        let result = tokenizer.tokenize_mapped_budgeted(
            &input,
            KuromojiLimits::default(),
            &budget,
            &mut || Ok(()),
        );
        match result {
            Ok(output) => {
                assert_eq!(*output, *expected);
                assert!(budget.peak() <= allowance + 7);
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
            Err(error) => panic!("allowance {allowance}: {error}"),
        }
        assert_eq!(budget.used(), 7);
        assert_eq!(format!("{input:?}"), before);
        drop(other);
    }
    let budget = MemoryBudget::new(baseline.peak() + 7);
    let other = budget.reserve(7).unwrap();
    for stop in [1, 2, polls / 3, polls / 2, polls - 2, polls - 1, polls] {
        let mut calls = 0;
        assert!(
            matches!(
                tokenizer.tokenize_mapped_budgeted(
                    &input,
                    KuromojiLimits::default(),
                    &budget,
                    &mut || {
                        calls += 1;
                        if calls == stop {
                            Err(AnalysisError::Cancelled)
                        } else {
                            Ok(())
                        }
                    }
                ),
                Err(AnalysisError::Cancelled)
            ),
            "poll {stop}"
        );
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), 7);
        assert_eq!(format!("{input:?}"), before);
    }
    let actual = tokenizer
        .tokenize_mapped_budgeted(&input, KuromojiLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, *expected);
    assert_eq!(format!("{input:?}"), before);
    drop(actual);
    assert_eq!(budget.used(), 7);
    drop(other);
    drop(expected);
    assert_eq!(baseline.used(), 0);
}

fn payload_bytes(output: AnalyzedText) -> usize {
    let mut bytes = output.batch.tokens.capacity() * size_of::<AnalysisToken>();
    if output.batch.terminal.is_some() {
        bytes += size_of::<AnalysisToken>();
    }
    for token in output
        .batch
        .tokens
        .into_iter()
        .chain(output.batch.terminal.map(|token| *token))
    {
        let attributes = token.japanese_morphology().unwrap();
        bytes += [
            &attributes.part_of_speech,
            &attributes.base_form,
            &attributes.reading,
            &attributes.pronunciation,
            &attributes.inflection_type,
            &attributes.inflection_form,
        ]
        .into_iter()
        .flatten()
        .map(String::capacity)
        .sum::<usize>();
        bytes += if token.term.as_str().is_some() {
            token.term.into_string().unwrap().capacity()
        } else {
            token.term.into_utf16().capacity() * size_of::<u16>()
        };
    }
    bytes
}

#[test]
fn japanese_attribute_copies_reserve_every_buffer_and_roll_back_every_failed_copy() {
    let input = attributes()
        .into_analyzed(&FilteredText::new("🙂a"))
        .unwrap();
    let baseline = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let output = input
        .clone_budgeted(&baseline, || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    let peak = baseline.peak();
    let (output, memory) = output.into_parts();
    assert_ne!(
        output.tokens()[0].term().utf16().as_ptr(),
        input.tokens()[0].term().utf16().as_ptr()
    );
    assert_eq!(output, input);
    assert_eq!(payload_bytes(output), memory.bytes());
    drop(memory);
    assert_eq!(baseline.used(), 0);
    for allowance in 0..peak {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        assert!(matches!(
            input.clone_budgeted(&budget, || Ok(())),
            Err(AnalysisError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    for stop in 1..=polls {
        let budget = MemoryBudget::new(peak + 7);
        let other = budget.reserve(7).unwrap();
        let mut calls = 0;
        assert!(matches!(
            input.clone_budgeted(&budget, || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(AnalysisError::Cancelled)
        ));
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), 7);
        drop(other);
    }
}

#[test]
fn japanese_conversion_rejects_mismatched_input_spans_and_invalid_graphs() {
    let input = FilteredText::new("🙂a");
    let mut output = attributes();
    output.final_offset_utf16 = 4;
    assert!(matches!(
        output.into_analyzed(&input),
        Err(AnalysisError::MismatchedAnalysisInput { .. })
    ));
    let mut output = attributes();
    output.tokens[0].end_utf16 = 4;
    assert!(matches!(
        output.into_analyzed(&input),
        Err(AnalysisError::InvalidTextOffset { .. })
    ));
    let mut output = attributes();
    output.tokens[0].position_increment = 0;
    assert!(matches!(
        output.into_analyzed(&input),
        Err(AnalysisError::InvalidTokenPosition)
    ));
    let mut output = attributes();
    output.tokens[0].position_length = 0;
    assert!(matches!(
        output.into_analyzed(&input),
        Err(AnalysisError::InvalidTokenPosition)
    ));
}

#[cfg(feature = "nori")]
#[test]
fn korean_filters_treat_japanese_attributes_as_absent() {
    use crate::nori::{KoreanFilter, NoriResources};
    let model = NoriResources::default().load_default().unwrap();
    let input = attributes()
        .into_analyzed(&FilteredText::new("🙂a"))
        .unwrap();
    for filter in [
        KoreanFilter::PartOfSpeech { stop_tags: None },
        KoreanFilter::ReadingForm,
    ] {
        assert_eq!(
            filter
                .filter_analyzed(input.clone(), model.model())
                .unwrap(),
            input
        );
    }
}

#[cfg(feature = "nori")]
#[test]
fn japanese_completion_clears_foreign_morphology_and_retains_corrected_source() {
    use crate::kuromoji::{CompletionMode, JapaneseFilter};
    use crate::nori::{KoreanMorphology, NoriOrigin, POSTag, POSType};
    let model = KuromojiResources::default().load_default().unwrap();
    let mut source = crate::Tokenizer::Keyword
        .tokenize_with_offsets("ア")
        .unwrap();
    source.batch.tokens[0].morphology = Some(Morphology::Korean(KoreanMorphology {
        pos_type: POSType::Morpheme,
        left_pos: POSTag::NNG,
        right_pos: POSTag::NNG,
        reading: Some("foreign reading".into()),
        morphemes: None,
        origin: NoriOrigin::Known,
    }));
    let filter = JapaneseFilter::Completion {
        mode: CompletionMode::Index,
    };
    let output = filter.filter_analyzed(source, model.model()).unwrap();
    assert_eq!(
        output
            .tokens()
            .iter()
            .map(|token| token.term().as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ア", "a"]
    );
    for token in output.tokens() {
        assert!(token.morphology.is_none());
        assert_eq!(token.offsets().unwrap().utf16, 0..1);
    }
}

#[cfg(feature = "nori")]
#[test]
fn later_number_composition_cannot_publish_invalid_japanese_terminal_attributes() {
    use crate::kuromoji::{JapaneseAnalyzer, JapaneseFilter};

    let model = KuromojiResources::default()
        .load_default()
        .unwrap()
        .model()
        .clone();
    let user = crate::kuromoji::UserDictionary::compile(
        "東京,東京,トウキョウ,",
        &model,
        crate::kuromoji::UserDictionaryLimits::default(),
    )
    .unwrap();
    let analyzer = JapaneseAnalyzer::with_filters(
        model,
        user,
        KuromojiOptions::default(),
        &[JapaneseFilter::Stop {
            words: Some(vec!["東京".into()]),
            ignore_case: false,
        }],
    )
    .unwrap();
    let input = analyzer.analyze("1 東京").unwrap();
    assert_eq!(input.tokens().len(), 1);
    let korean = crate::nori::NoriResources::default()
        .load_default()
        .unwrap();
    assert!(matches!(
        crate::nori::KoreanFilter::Number.filter_analyzed(input.clone(), korean.model()),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
    let filter: crate::TokenFilter = serde_json::from_str(r#"{"type":"nori_number"}"#).unwrap();
    assert!(matches!(
        filter.filter_analyzed(input.clone()),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
    let budget = MemoryBudget::new(1 << 20);
    let held = budget.reserve(7).unwrap();
    let reserved = input.clone_budgeted(&budget, || Ok(())).unwrap();
    assert!(matches!(
        filter.filter_analyzed_budgeted(reserved, || Ok(())),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
    assert_eq!(budget.used(), 7);
    drop(held);
}
