//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;
use crate::{AnalysisError, CharFilter, TokenFilter, Tokenizer};
use uqa_core::memory::MemoryError;

fn verify(
    mut run: impl FnMut(
        &MemoryBudget,
        &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>>,
    expected: &str,
) {
    let baseline = MemoryBudget::new(1 << 24);
    let mut calls = 0;
    let output = run(&baseline, &mut || {
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(&**output, expected);
    assert_eq!(baseline.used(), output.capacity());
    let peak = baseline.peak();
    drop(output);
    assert_eq!(baseline.used(), 0);
    for allowance in [
        0,
        1,
        32,
        peak / 4,
        peak / 2,
        peak.saturating_sub(1),
        peak,
        peak + 1,
    ] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match run(&budget, &mut || Ok(())) {
            Ok(output) => {
                assert_eq!(&**output, expected);
                assert_eq!(budget.used(), output.capacity() + 7);
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => assert!(allowance < peak),
            result => panic!("allowance={allowance}: {result:?}"),
        }
        assert_eq!(budget.used(), 7);
        assert!(budget.peak() <= budget.limit());
        drop(other);
    }
    for stop in 1..=calls {
        let budget = MemoryBudget::new(1 << 24);
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
            "callback={stop}: {result:?}"
        );
        assert_eq!(budget.used(), 7);
        drop(other);
    }
}

#[test]
fn public_highlighters_release_all_runtime_state_on_limits_and_cancellation() {
    let options = HighlightOptions {
        max_fragments: 2,
        fragment_size: 5,
        ..Default::default()
    };
    let analyzer = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 1,
            max_gram: 3,
        },
        vec![TokenFilter::Lowercase],
        vec![CharFilter::HTMLStrip],
    );
    let source = "<b>한&amp;🙂</b> fox xx FOX";
    let queries = ["fox".into(), "FOX".into(), "한".into()];
    let compiled = analyzer.compile().unwrap();
    let expected =
        super::reference::highlight_compiled(source, &queries, &compiled, &options).unwrap();
    verify(
        |budget, poll| {
            highlight_compiled_budgeted(source, &queries, &compiled, &options, budget, poll)
        },
        &expected,
    );
    verify(
        |budget, poll| {
            highlight_budgeted(source, &queries, Some(&analyzer), &options, budget, poll)
        },
        &expected,
    );
    for analyzer in [None, Some(crate::standard_analyzer("english"))] {
        let expected =
            super::reference::highlight_words(source, &queries, analyzer.as_ref(), &options)
                .unwrap();
        verify(
            |budget, poll| {
                highlight_words_budgeted(
                    source,
                    queries.iter().map(String::as_str),
                    analyzer.as_ref(),
                    &options,
                    budget,
                    poll,
                )
            },
            &expected,
        );
    }
    for (source, queries) in [
        ("", vec!["a".into()]),
        ("abc", vec![]),
        ("long source", vec!["none".into()]),
    ] {
        let expected = super::reference::highlight(source, &queries, None, &options).unwrap();
        verify(
            |budget, poll| highlight_budgeted(source, &queries, None, &options, budget, poll),
            &expected,
        );
    }
}

#[test]
fn long_word_scans_and_tag_copies_are_interruptible_between_bounded_chunks() {
    let text = "𐐀".repeat(4096);
    let options = HighlightOptions {
        start_tag: "〈🙂".repeat(4096),
        end_tag: "〉".repeat(4096),
        ..Default::default()
    };
    let expected = format!("{}{text}{}", options.start_tag, options.end_tag);
    verify(
        |budget, poll| highlight_words_budgeted(&text, [&*text], None, &options, budget, poll),
        &expected,
    );
}

#[test]
fn uncompiled_word_analysis_reloads_synonyms_between_query_and_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "nyc, metropolitan").unwrap();
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::synonym_from_path(&path).unwrap()],
        Vec::new(),
    );
    assert_eq!(
        highlight_words(
            "nyc",
            &["urban".into()],
            Some(&analyzer),
            &HighlightOptions::default()
        )
        .unwrap(),
        "nyc"
    );
    let inputs = std::iter::once("urban").chain(std::iter::from_fn(|| {
        std::fs::write(&path, "nyc, urban").unwrap();
        None
    }));
    let budget = MemoryBudget::new(1 << 20);
    let result = highlight_words_budgeted(
        "nyc",
        inputs,
        Some(&analyzer),
        &HighlightOptions::default(),
        &budget,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(&**result, "<b>nyc</b>");
    assert_eq!(budget.used(), result.capacity());
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[cfg(feature = "nori")]
#[test]
fn raw_query_terms_keep_identity_and_legacy_words_validate_all_tokens_before_a_hit() {
    let analyzer: Analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"discard","user_dictionary":"🙂a 가 나\na𐐀 가 나 다"},"char_filters":[{"type":"html_strip"}],"token_filters":[]}"#).unwrap();
    let compiled = analyzer.compile().unwrap();
    let opts = HighlightOptions::default();
    verify(
        |budget, poll| {
            highlight_compiled_budgeted(
                "<i>🙂a</i> �a",
                &["🙂a".into()],
                &compiled,
                &opts,
                budget,
                poll,
            )
        },
        "<i><b>🙂</b><b>a</b></i> �a",
    );
    let tokens = analyzer.analyze_tokens("a𐐀").unwrap();
    assert_eq!(tokens.tokens()[0].term().as_str(), Some("a"));
    assert!(tokens
        .tokens()
        .iter()
        .skip(1)
        .any(|token| token.term().as_str().is_none()));
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let result =
        highlight_words_budgeted("a𐐀", ["a"], Some(&analyzer), &opts, &budget, || Ok(()));
    assert!(
        matches!(result, Err(AnalysisError::UnpairedTokenSurrogate { .. })),
        "{result:?}"
    );
    assert_eq!(budget.used(), 7);
    drop(other);
}
