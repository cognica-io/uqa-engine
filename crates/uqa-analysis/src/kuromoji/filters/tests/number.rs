//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::controls::{copy, retained};
use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::{
    normalize_number_budgeted, normalize_number_utf16_budgeted, DictionaryError, JapaneseAnalyzer,
    JapaneseFilter, JapaneseTokenizer, KuromojiLimits, KuromojiOptions, UserDictionary,
    UserDictionaryLimits,
};
use crate::{AnalysisError, AnalysisResult, AnalyzedText, CharFilter, Tokenizer};
use std::fmt::Debug;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};

fn audit<T: Debug + PartialEq>(
    run: impl Fn(&MemoryBudget, &mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Budgeted<T>>,
    retained: impl Fn(&T) -> usize,
) {
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let expected = run(&baseline, &mut || {
        polls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(expected.reserved_bytes(), retained(&expected));
    assert_eq!(baseline.used(), expected.reserved_bytes());
    let peak = baseline.peak();
    for stop in 1..=polls {
        let budget = MemoryBudget::new(peak + 7);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        let result = run(&budget, &mut || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "poll {stop}/{polls}: {result:?}"
        );
        assert_eq!(count, stop);
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    for allowance in [0, 1, peak / 2, peak - 1, peak] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match run(&budget, &mut || Ok(())) {
            Ok(actual) => {
                assert_eq!(*actual, *expected);
                assert_eq!(actual.reserved_bytes(), retained(&actual));
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => assert!(allowance < peak),
            result => panic!("allowance {allowance}/{peak}: {result:?}"),
        }
        assert!(budget.peak() <= budget.limit());
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    drop(expected);
    assert_eq!(baseline.used(), 0);
    drop(run(&baseline, &mut || Ok(())).unwrap());
    assert_eq!(baseline.used(), 0);
}

#[test]
fn japanese_numbers_reserve_exact_results_and_cancel_coefficients_and_fallbacks() {
    for text in [
        format!("{}十1", "9".repeat(2049)),
        format!("{}..", "一".repeat(2049)),
        "壱二〇".into(),
    ] {
        audit(
            |budget, poll| {
                normalize_number_budgeted(&text, KuromojiLimits::default(), budget, &mut || poll())
            },
            String::capacity,
        );
        let units: Vec<_> = text.encode_utf16().collect();
        audit(
            |budget, poll| {
                normalize_number_utf16_budgeted(
                    &units,
                    KuromojiLimits::default(),
                    budget,
                    &mut || poll(),
                )
            },
            |output| output.capacity() * size_of::<u16>(),
        );
    }
    let raw = [0xd800, 0xff11, 0xdc00];
    let budget = MemoryBudget::new(1024);
    let output =
        normalize_number_utf16_budgeted(&raw, KuromojiLimits::default(), &budget, &mut || Ok(()))
            .unwrap();
    assert_eq!(*output, raw);
    drop(output);
    assert_eq!(budget.used(), 0);
}

fn common_retained(output: &AnalyzedText) -> usize {
    let mut bytes = output.batch.tokens.capacity() * size_of::<crate::AnalysisToken>();
    for token in &output.batch.tokens {
        bytes += token.allocation_bytes(&mut || Ok(())).unwrap();
    }
    if let Some(token) = &output.batch.terminal {
        bytes +=
            size_of::<crate::AnalysisToken>() + token.allocation_bytes(&mut || Ok(())).unwrap();
    }
    bytes
}

#[test]
fn japanese_number_streams_release_lookahead_scratch_and_terminal_leases() {
    let model = model();
    let tokenizer =
        JapaneseTokenizer::new(model.clone(), None, KuromojiOptions::default()).unwrap();
    for text in [
        "十 万 東京".into(),
        format!("{} 十 円", "9".repeat(2049)),
        "一 . . 二 円".into(),
    ] {
        let source = tokenizer.tokenize(&text).unwrap();
        audit(
            |budget, poll| {
                JapaneseFilter::Number.apply_budgeted(
                    copy(&source, budget)?,
                    &model,
                    KuromojiLimits::default(),
                    &mut || poll(),
                )
            },
            retained,
        );
        let common = source
            .into_analyzed(&crate::FilteredText::new(&text))
            .unwrap();
        audit(
            |budget, poll| {
                JapaneseFilter::Number.filter_analyzed_budgeted(
                    common.clone_budgeted(budget, &mut *poll)?,
                    &model,
                    KuromojiLimits::default(),
                    &mut || poll(),
                )
            },
            common_retained,
        );
    }
    let mut source = tokenizer.tokenize("一 二 三 四").unwrap();
    assert_eq!(source.tokens.len(), 4);
    source.tokens[2].position_increment = 0;
    source.tokens[2].position_length = 1;
    audit(
        |budget, poll| {
            JapaneseFilter::Number.apply_budgeted(
                copy(&source, budget)?,
                &model,
                KuromojiLimits::default(),
                &mut || poll(),
            )
        },
        retained,
    );
    source.tokens[3].position_increment = 0;
    let stop = JapaneseFilter::Stop {
        words: Some(vec!["四".into()]),
        ignore_case: false,
    };
    let source = stop.apply(source, &model).unwrap();
    assert!(source.terminal.is_some());
    audit(
        |budget, poll| {
            JapaneseFilter::Number.apply_budgeted(
                copy(&source, budget)?,
                &model,
                KuromojiLimits::default(),
                &mut || poll(),
            )
        },
        retained,
    );
    let common = source
        .into_analyzed(&crate::FilteredText::new("一 二 三 四"))
        .unwrap();
    audit(
        |budget, poll| {
            JapaneseFilter::Number.filter_analyzed_budgeted(
                common.clone_budgeted(budget, &mut *poll)?,
                &model,
                KuromojiLimits::default(),
                &mut || poll(),
            )
        },
        common_retained,
    );
}

fn limit(error: AnalysisError, expected: &str) {
    match error {
        AnalysisError::KuromojiDictionary(DictionaryError::Limit {
            resource,
            required,
            limit,
        }) => {
            assert_eq!(resource, expected);
            assert!(required > limit);
        }
        error => panic!("unexpected limit error: {error}"),
    }
}

#[test]
fn japanese_number_normalization_limits_keep_typed_errors() {
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    for (text, limits, resource) in [
        (
            "百",
            KuromojiLimits {
                max_input_utf16: 0,
                ..KuromojiLimits::default()
            },
            "Kuromoji input UTF-16 units",
        ),
        (
            "垓",
            KuromojiLimits {
                max_output_utf16: 20,
                ..KuromojiLimits::default()
            },
            "Kuromoji numeric units",
        ),
        (
            "1.2.3",
            KuromojiLimits {
                max_output_utf16: 4,
                ..KuromojiLimits::default()
            },
            "Kuromoji numeric units",
        ),
    ] {
        limit(
            normalize_number_budgeted(text, limits, &budget, &mut || Ok(())).unwrap_err(),
            resource,
        );
        assert_eq!(budget.used(), 7);
    }
    drop(other);
}

#[test]
fn japanese_number_stream_limits_include_all_six_morphology_fields() {
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let model = model();
    let source = JapaneseTokenizer::new(model.clone(), None, KuromojiOptions::default())
        .unwrap()
        .tokenize("一")
        .unwrap();
    let mut sample = source.tokens[0].clone();
    sample.term_utf16 = vec![u16::from(b'A')];
    sample.part_of_speech = Some("品詞".into());
    sample.base_form = Some("基本".into());
    sample.reading = Some("ヨミ".into());
    sample.pronunciation = Some("ハツオン".into());
    sample.inflection_type = Some("種類".into());
    sample.inflection_form = Some("活用".into());
    let total = 1 + sample
        .fields()
        .iter()
        .flatten()
        .map(|field| field.encode_utf16().count())
        .sum::<usize>();
    let input = crate::kuromoji::KuromojiOutput::from_tokens(vec![sample], 1, 0);
    for (limits, resource) in [
        (
            KuromojiLimits {
                max_input_utf16: 0,
                ..KuromojiLimits::default()
            },
            "Kuromoji input UTF-16 units",
        ),
        (
            KuromojiLimits {
                max_tokens: 0,
                ..KuromojiLimits::default()
            },
            "Kuromoji output tokens",
        ),
        (
            KuromojiLimits {
                max_output_utf16: total - 1,
                ..KuromojiLimits::default()
            },
            "Kuromoji output UTF-16 units",
        ),
    ] {
        limit(
            JapaneseFilter::Number
                .apply_budgeted(copy(&input, &budget).unwrap(), &model, limits, &mut || {
                    Ok(())
                })
                .unwrap_err(),
            resource,
        );
        assert_eq!(budget.used(), 7);
    }
    let output = JapaneseFilter::Number
        .apply_budgeted(
            copy(&input, &budget).unwrap(),
            &model,
            KuromojiLimits {
                max_output_utf16: total,
                ..KuromojiLimits::default()
            },
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(*output, input);
    drop(output);
    assert_eq!(budget.used(), 7);
    drop(other);
}

#[test]
fn japanese_number_spans_keep_original_source_owners_and_normalization() {
    let model = model();
    assert_eq!(
        serde_json::from_str::<JapaneseFilter>(r#"{"type":"kuromoji_number"}"#).unwrap(),
        JapaneseFilter::Number
    );
    assert_eq!(
        serde_json::to_string(&JapaneseFilter::Number).unwrap(),
        r#"{"type":"kuromoji_number"}"#
    );
    let source_budget = MemoryBudget::new(1 << 20);
    let source = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>３ 百</b>", &source_budget, &mut || Ok(()))
        .unwrap();
    let analyzer = JapaneseAnalyzer::with_filters(
        model.clone(),
        None,
        KuromojiOptions::default(),
        &[JapaneseFilter::Number],
    )
    .unwrap();
    let runtime = MemoryBudget::new(1 << 20);
    let output = analyzer
        .analyze_mapped_budgeted(&source, KuromojiLimits::default(), &runtime, &mut || Ok(()))
        .unwrap();
    assert_eq!(output.tokens().len(), 1);
    let token = &output.tokens()[0];
    assert_eq!(token.term(), "300");
    assert_eq!(token.offsets().unwrap().utf16, 3..6);
    assert_eq!(token.offsets().unwrap().utf8, 3..10);
    assert_eq!(token.substring(0..1).offsets(), token.offsets());
    assert_eq!(output.final_offsets().utf16.end, 10);
    drop(source);
    assert!(source_budget.used() > 0);
    drop(output);
    assert_eq!(source_budget.used(), 0);
    assert_eq!(runtime.used(), 0);
    assert_eq!(analyzer.normalize("３ 百").unwrap(), "3 百");
    let common = JapaneseFilter::Number
        .filter_analyzed(
            Tokenizer::Whitespace
                .tokenize_with_offsets("十 万 円")
                .unwrap(),
            &model,
        )
        .unwrap();
    assert_eq!(common.tokens()[0].term(), "100000");
    assert!(common
        .tokens()
        .iter()
        .all(|token| token.japanese_morphology().is_none()));
}

#[test]
fn japanese_number_rejects_invalid_terminal_attributes_promoted_into_output() {
    let model = model();
    let user = UserDictionary::compile(
        "東京,東京,トウキョウ,",
        &model,
        UserDictionaryLimits::default(),
    )
    .unwrap();
    let analyzer = JapaneseAnalyzer::with_filters(
        model.clone(),
        user,
        KuromojiOptions::default(),
        &[JapaneseFilter::Stop {
            words: Some(vec!["東京".into()]),
            ignore_case: false,
        }],
    )
    .unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let input = analyzer
        .analyze_budgeted("十東京", KuromojiLimits::default(), &budget, &mut || {
            Ok(())
        })
        .unwrap();
    assert_eq!(input.tokens().len(), 1);
    assert!(input.batch.terminal.is_some());
    assert!(matches!(
        JapaneseFilter::Number.filter_analyzed_budgeted(
            input,
            &model,
            KuromojiLimits::default(),
            &mut || Ok(())
        ),
        Err(AnalysisError::KuromojiDictionary(
            DictionaryError::Invalid {
                section: "user dictionary",
                ..
            }
        ))
    ));
    assert_eq!(budget.used(), 7);
    drop(other);
}
