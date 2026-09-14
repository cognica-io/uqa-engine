//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::{
    JapaneseAnalyzer, JapaneseFilter, JapaneseTokenizer, KuromojiLimits, KuromojiMode,
    KuromojiOptions, KuromojiOutput,
};
use crate::token::allocation::{AllocatedToken, TokenBuffer};
use crate::{AnalysisError, AnalysisResult, CharFilter, Tokenizer};
use uqa_core::memory::{Budgeted, MemoryBudget};

fn filters() -> Vec<JapaneseFilter> {
    vec![
        JapaneseFilter::BaseForm,
        JapaneseFilter::PartOfSpeech { stop_tags: None },
        JapaneseFilter::Stop {
            words: None,
            ignore_case: true,
        },
        JapaneseFilter::KatakanaStem { minimum_length: 4 },
        JapaneseFilter::SimpleLowercase,
    ]
}
fn copy(input: &KuromojiOutput, budget: &MemoryBudget) -> AnalysisResult<Budgeted<KuromojiOutput>> {
    let mut buffer = TokenBuffer::new(budget);
    buffer.reserve_tokens(input.tokens.len())?;
    for token in &input.tokens {
        buffer.push(token.clone_budgeted(budget, &mut || Ok(()))?)?;
    }
    if let Some(terminal) = &input.terminal {
        buffer.set_terminal_token(terminal.clone_budgeted(budget, &mut || Ok(()))?)?;
    }
    let (batch, memory) = buffer
        .into_batch(input.final_position_increment)
        .into_parts();
    let mut output = KuromojiOutput::from_tokens(
        batch.tokens,
        input.final_offset_utf16,
        batch.final_position_increment,
    );
    output.terminal = batch.terminal;
    Ok(Budgeted::new(output, memory))
}
fn retained(output: &KuromojiOutput) -> usize {
    let mut bytes = output.tokens.capacity() * size_of::<crate::kuromoji::KuromojiToken>();
    for token in &output.tokens {
        bytes += token.allocation_bytes(&mut || Ok(())).unwrap();
    }
    if let Some(terminal) = &output.terminal {
        bytes += size_of::<crate::kuromoji::KuromojiToken>()
            + terminal.allocation_bytes(&mut || Ok(())).unwrap();
    }
    bytes
}

#[test]
fn japanese_filter_memory_and_cancellation_preserve_other_reservations() {
    let model = model();
    let tokenizer =
        JapaneseTokenizer::new(model.clone(), None, KuromojiOptions::default()).unwrap();
    let source = tokenizer.tokenize("UQAで走りました シャワーです").unwrap();
    let limits = KuromojiLimits::default();
    for filter in filters() {
        let baseline = MemoryBudget::new(usize::MAX);
        let mut polls = 0;
        let expected = filter
            .apply_budgeted(
                copy(&source, &baseline).unwrap(),
                &model,
                limits,
                &mut || {
                    polls += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(expected.reserved_bytes(), retained(&expected));
        assert_eq!(baseline.used(), expected.reserved_bytes());
        for stop in [1, 2, polls / 2, polls - 1, polls] {
            let budget = MemoryBudget::new(baseline.peak() + 7);
            let held = budget.reserve(7).unwrap();
            let mut calls = 0;
            let input = copy(&source, &budget).unwrap();
            assert!(
                matches!(
                    filter.apply_budgeted(input, &model, limits, &mut || {
                        calls += 1;
                        if calls == stop {
                            Err(AnalysisError::Cancelled)
                        } else {
                            Ok(())
                        }
                    }),
                    Err(AnalysisError::Cancelled)
                ),
                "{filter:?} {stop}/{polls}"
            );
            assert_eq!(calls, stop);
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        for allowance in [0, 1, baseline.peak() / 2, baseline.peak() - 1] {
            let budget = MemoryBudget::new(allowance + 7);
            let held = budget.reserve(7).unwrap();
            match copy(&source, &budget)
                .and_then(|input| filter.apply_budgeted(input, &model, limits, &mut || Ok(())))
            {
                Ok(actual) => {
                    assert_eq!(*actual, *expected);
                    assert!(budget.peak() <= allowance + 7);
                }
                Err(AnalysisError::Memory(_)) => {}
                Err(error) => panic!("{filter:?}: {error}"),
            }
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        drop(expected);
        assert_eq!(baseline.used(), 0);
    }
}

#[test]
fn japanese_filter_configuration_bounds_and_absent_attributes_are_explicit() {
    let model = model();
    let input = Tokenizer::Whitespace
        .tokenize_with_offsets("UQA シャワー")
        .unwrap();
    let output = JapaneseFilter::BaseForm
        .filter_analyzed(input, &model)
        .unwrap();
    let output = JapaneseFilter::PartOfSpeech { stop_tags: None }
        .filter_analyzed(output, &model)
        .unwrap();
    let output = JapaneseFilter::KatakanaStem { minimum_length: 4 }
        .filter_analyzed(output, &model)
        .unwrap();
    assert_eq!(output.tokens()[0].term(), "UQA");
    assert_eq!(output.tokens()[1].term(), "シャワ");
    assert!(output
        .tokens()
        .iter()
        .all(|token| token.japanese_morphology().is_none()));
    for invalid in [
        r#"{"type":"kuromoji_baseform","unknown":true}"#,
        r#"{"type":"kuromoji_stop","ignore_case":"yes"}"#,
        r#"{"type":"kuromoji_stemmer","minimum_length":null}"#,
    ] {
        assert!(serde_json::from_str::<JapaneseFilter>(invalid).is_err());
    }
    let tokenizer =
        JapaneseTokenizer::new(model.clone(), None, KuromojiOptions::default()).unwrap();
    for limits in [
        KuromojiLimits {
            max_tokens: 0,
            ..KuromojiLimits::default()
        },
        KuromojiLimits {
            max_input_utf16: 0,
            ..KuromojiLimits::default()
        },
        KuromojiLimits {
            max_output_utf16: 0,
            ..KuromojiLimits::default()
        },
        KuromojiLimits {
            max_filter_entries: 0,
            ..KuromojiLimits::default()
        },
        KuromojiLimits {
            max_filter_utf16: 0,
            ..KuromojiLimits::default()
        },
    ] {
        let budget = MemoryBudget::new(1 << 20);
        let held = budget.reserve(7).unwrap();
        let input = tokenizer
            .tokenize_budgeted("UQA", KuromojiLimits::default(), &budget, &mut || Ok(()))
            .unwrap();
        assert!(matches!(
            JapaneseFilter::Stop {
                words: None,
                ignore_case: true
            }
            .apply_budgeted(input, &model, limits, &mut || Ok(())),
            Err(AnalysisError::KuromojiDictionary(_))
        ));
        assert_eq!(budget.used(), 7);
        drop(held);
    }
}

#[test]
fn japanese_analyzer_preserves_mapped_sources_and_separates_normalization() {
    let model = model();
    let preparation = MemoryBudget::new(1 << 20);
    let analyzer = JapaneseAnalyzer::with_filters_budgeted(
        model.clone(),
        None,
        KuromojiOptions::default(),
        &filters(),
        KuromojiLimits::default(),
        &preparation,
        &mut || Ok(()),
    )
    .unwrap();
    assert!(preparation.used() > 0);
    let source_budget = MemoryBudget::new(1 << 20);
    let input = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>ｼｬﾜｰで走りました</b>", &source_budget, &mut || Ok(()))
        .unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let result = analyzer
        .analyze_mapped_budgeted(&input, KuromojiLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(result.tokens()[0].term(), "シャワ");
    assert_eq!(result.tokens()[0].offsets().unwrap().utf16, 3..7);
    assert_eq!(result.tokens()[1].term(), "走る");
    assert_eq!(
        result.final_offsets().utf16.end,
        "<b>ｼｬﾜｰで走りました</b>".encode_utf16().count()
    );
    drop(input);
    assert!(source_budget.used() > 0);
    drop(result);
    assert_eq!(budget.used(), 0);
    assert_eq!(source_budget.used(), 0);
    assert_eq!(
        analyzer.normalize("ｼｬﾜｰで走りました UQA").unwrap(),
        "シャワーで走りました uqa"
    );
    let plain = JapaneseAnalyzer::new(model, None, KuromojiMode::Search).unwrap();
    assert_eq!(plain.analyze("ｼｬﾜｰで走りました").unwrap().tokens().len(), 2);
    drop(analyzer);
    assert_eq!(preparation.used(), 0);
}
