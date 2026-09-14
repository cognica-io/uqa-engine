//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::controls::{copy, retained};
use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::{
    CompletionMode, JapaneseAnalyzer, JapaneseFilter, KuromojiLimits, KuromojiOrigin,
    KuromojiOutput, KuromojiToken,
};
use crate::{AnalysisError, AnalysisResult, CharFilter, FilteredText};
use uqa_core::memory::{Budgeted, MemoryBudget};

fn input(count: usize) -> KuromojiOutput {
    let tokens = (0..count)
        .map(|index| {
            let mut token =
                KuromojiToken::new(vec![0x30a2], index..index + 1, KuromojiOrigin::Known);
            token.keyword = true;
            token.position_length = 4;
            token.position_increment = 3;
            token.part_of_speech = Some("名詞".into());
            token
        })
        .collect();
    KuromojiOutput::from_tokens(tokens, count, 7)
}

#[test]
fn completion_recreates_positions_without_dictionary_attributes_or_origins() {
    let model = model();
    let source = input(257);
    for mode in [CompletionMode::Index, CompletionMode::Query] {
        let filter = JapaneseFilter::Completion { mode };
        let limits = KuromojiLimits {
            max_completion_work: 20_000,
            ..KuromojiLimits::default()
        };
        let output = filter
            .apply_controlled(source.clone(), &model, limits, &mut || Ok(()))
            .unwrap();
        assert_eq!(
            output.tokens.len(),
            if mode == CompletionMode::Index {
                514
            } else {
                2
            }
        );
        assert_eq!(output.final_position_increment, 7);
        for (index, token) in output.tokens.iter().enumerate() {
            assert_eq!(token.position_increment, u32::from(index % 2 == 0));
            assert_eq!(token.position_length, 1);
            assert!(!token.keyword);
            assert!(token.origin.is_none());
            assert!(token.fields().into_iter().all(|field| field.is_none()));
        }
        let text = "ア".repeat(257);
        let bridge = output.into_analyzed(&FilteredText::new(&text)).unwrap();
        let common = filter
            .filter_analyzed(
                source
                    .clone()
                    .into_analyzed(&FilteredText::new(&text))
                    .unwrap(),
                &model,
            )
            .unwrap();
        assert_eq!(bridge.tokens(), common.tokens());
        assert!(common
            .tokens()
            .iter()
            .all(|token| token.japanese_morphology().is_none()));
        if mode == CompletionMode::Query {
            assert_eq!(common.tokens()[0].term(), text.as_str());
            assert_eq!(common.tokens()[1].term(), "a".repeat(257).as_str());
            assert_eq!(common.tokens()[1].offsets().unwrap().utf16, 0..257);
        }
    }
}

fn audit<T: std::fmt::Debug + PartialEq>(
    run: impl Fn(&MemoryBudget, &mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Budgeted<T>>,
    retained: impl Fn(&T) -> usize,
) {
    let budget = MemoryBudget::new(usize::MAX);
    let mut calls = 0;
    let expected = run(&budget, &mut || {
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(expected.reserved_bytes(), retained(&expected));
    assert_eq!(budget.used(), expected.reserved_bytes());
    let peak = budget.peak();
    for stop in 1..=calls {
        let trial = MemoryBudget::new(peak + 7);
        let held = trial.reserve(7).unwrap();
        let mut count = 0;
        let result = run(&trial, &mut || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "{stop}/{calls}: {result:?}"
        );
        assert_eq!(count, stop);
        assert_eq!(trial.used(), 7);
        drop(held);
    }
    for allowance in [0, 1, peak / 2, peak - 1, peak] {
        let trial = MemoryBudget::new(allowance + 7);
        let held = trial.reserve(7).unwrap();
        match run(&trial, &mut || Ok(())) {
            Ok(actual) => {
                assert_eq!(*actual, *expected);
                assert_eq!(actual.reserved_bytes(), retained(&actual));
            }
            Err(AnalysisError::Memory(_)) => assert!(allowance < peak),
            result => panic!("{allowance}/{peak}: {result:?}"),
        }
        assert_eq!(trial.used(), 7);
        assert!(trial.peak() <= trial.limit());
        drop(held);
    }
    drop(expected);
    assert_eq!(budget.used(), 0);
    drop(run(&budget, &mut || Ok(())).unwrap());
    assert_eq!(budget.used(), 0);
}

#[test]
fn completion_native_and_common_streams_retain_exact_leases_and_cancel_every_callback() {
    let model = model();
    let native = input(17);
    let text = "ア".repeat(17);
    let common = native
        .clone()
        .into_analyzed(&FilteredText::new(&text))
        .unwrap();
    for mode in [CompletionMode::Index, CompletionMode::Query] {
        let filter = JapaneseFilter::Completion { mode };
        audit(
            |budget, poll| {
                filter.apply_budgeted(
                    copy(&native, budget)?,
                    &model,
                    KuromojiLimits::default(),
                    &mut || poll(),
                )
            },
            retained,
        );
        audit(
            |budget, poll| {
                filter.filter_analyzed_budgeted(
                    common.clone_budgeted(budget, &mut *poll)?,
                    &model,
                    KuromojiLimits::default(),
                    &mut || poll(),
                )
            },
            |value| {
                value.batch.tokens.capacity() * size_of::<crate::AnalysisToken>()
                    + value
                        .tokens()
                        .iter()
                        .map(|token| token.allocation_bytes(&mut || Ok(())).unwrap())
                        .sum::<usize>()
            },
        );
    }
}

#[test]
fn completion_analyzer_retains_original_sources_and_normalizes_width_without_lowercase() {
    let model = model();
    let default: JapaneseFilter =
        serde_json::from_str(r#"{"type":"kuromoji_completion"}"#).unwrap();
    assert_eq!(
        default,
        JapaneseFilter::Completion {
            mode: CompletionMode::Index
        }
    );
    for invalid in [
        r#"{"type":"kuromoji_completion","mode":null}"#,
        r#"{"type":"kuromoji_completion","unknown":true}"#,
        r#"{"type":"kuromoji_completion","mode":"other"}"#,
    ] {
        assert!(serde_json::from_str::<JapaneseFilter>(invalid).is_err());
    }
    for mode in [CompletionMode::Index, CompletionMode::Query] {
        let filter = JapaneseFilter::Completion { mode };
        assert_eq!(
            serde_json::from_str::<JapaneseFilter>(&serde_json::to_string(&filter).unwrap())
                .unwrap(),
            filter
        );
        let preparation = MemoryBudget::new(1 << 20);
        let analyzer = JapaneseAnalyzer::completion_budgeted(
            model.clone(),
            None,
            mode,
            KuromojiLimits::default(),
            &preparation,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(preparation.used() > 0);
        let source_budget = MemoryBudget::new(1 << 20);
        let source = CharFilter::HTMLStrip
            .filter_with_offsets_budgeted("<b>ｶﾞ</b>", &source_budget, &mut || Ok(()))
            .unwrap();
        let runtime = MemoryBudget::new(1 << 20);
        let result = analyzer
            .analyze_mapped_budgeted(&source, KuromojiLimits::default(), &runtime, &mut || Ok(()))
            .unwrap();
        assert_eq!(
            result
                .tokens()
                .iter()
                .map(|token| token.term().as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ガ", "ga"]
        );
        for token in result.tokens() {
            assert_eq!(token.offsets().unwrap().utf16, 3..5);
            assert_eq!(token.offsets().unwrap().utf8, 3..9);
            assert!(token.japanese_morphology().is_none());
        }
        drop(source);
        assert!(source_budget.used() > 0);
        drop(result);
        assert_eq!(runtime.used(), 0);
        assert_eq!(source_budget.used(), 0);
        audit(
            |budget, poll| {
                analyzer.normalize_budgeted(
                    "ＵＱＡ ｶﾞ",
                    KuromojiLimits::default(),
                    budget,
                    &mut || poll(),
                )
            },
            String::capacity,
        );
        assert_eq!(analyzer.normalize("ＵＱＡ ｶﾞ").unwrap(), "UQA ガ");
        drop(analyzer);
        assert_eq!(preparation.used(), 0);
    }
}

#[test]
fn completion_limits_count_originals_alternatives_and_empty_token_work_together() {
    use crate::kuromoji::DictionaryError;
    let model = model();
    for mode in [CompletionMode::Index, CompletionMode::Query] {
        let filter = JapaneseFilter::Completion { mode };
        let latin =
            KuromojiOutput::from_tokens(vec![KuromojiToken::new(vec![65], 0..1, None)], 1, 0);
        let exact = KuromojiLimits {
            max_output_utf16: 1,
            ..KuromojiLimits::default()
        };
        assert_eq!(
            filter
                .apply_controlled(latin, &model, exact, &mut || Ok(()))
                .unwrap()
                .tokens[0]
                .term_utf16,
            [65]
        );
        let source = input(4);
        let expected = filter.apply(source.clone(), &model).unwrap();
        let exact = KuromojiLimits {
            max_output_utf16: expected
                .tokens
                .iter()
                .map(|token| token.term_utf16.len())
                .sum(),
            max_tokens: expected.tokens.len().max(source.tokens.len()),
            max_input_utf16: 4,
            ..KuromojiLimits::default()
        };
        assert_eq!(
            filter
                .apply_controlled(source.clone(), &model, exact, &mut || Ok(()))
                .unwrap(),
            expected
        );
        for limits in [
            KuromojiLimits {
                max_output_utf16: exact.max_output_utf16 - 1,
                ..exact
            },
            KuromojiLimits {
                max_tokens: exact.max_tokens - 1,
                ..exact
            },
            KuromojiLimits {
                max_input_utf16: 3,
                ..exact
            },
            KuromojiLimits {
                max_completion_work: 0,
                ..exact
            },
        ] {
            let budget = MemoryBudget::new(1 << 20);
            let held = budget.reserve(7).unwrap();
            let error = filter
                .apply_budgeted(copy(&source, &budget).unwrap(), &model, limits, &mut || {
                    Ok(())
                })
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    AnalysisError::KuromojiDictionary(DictionaryError::Limit { .. })
                ),
                "{error:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        let empty =
            KuromojiOutput::from_tokens(vec![KuromojiToken::new(vec![], 0..0, None); 8], 0, 0);
        let error = filter
            .apply_controlled(
                empty,
                &model,
                KuromojiLimits {
                    max_completion_work: 4,
                    ..KuromojiLimits::default()
                },
                &mut || Ok(()),
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                AnalysisError::KuromojiDictionary(DictionaryError::Limit {
                    resource: "Kuromoji completion work",
                    ..
                })
            ),
            "{error:?}"
        );
    }
}
