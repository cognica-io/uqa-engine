//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{Analyzer, CharFilter, TokenFilter, Tokenizer};
use uqa_core::{memory::MemoryError, TokenOffsets};

fn verify<T: std::fmt::Debug + PartialEq>(
    expected: &T,
    owned: impl Fn(&T) -> usize,
    mut run: impl FnMut(
        &MemoryBudget,
        &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> StorageBackendResult<Budgeted<T>>,
) {
    let baseline = MemoryBudget::new(1 << 24);
    let mut calls = 0;
    let output = run(&baseline, &mut || {
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(&*output, expected);
    assert_eq!(output.reserved_bytes(), owned(&output));
    assert_eq!(baseline.used(), output.reserved_bytes());
    let peak = baseline.peak();
    drop(output);
    assert_eq!(baseline.used(), 0);
    for allowance in [0, 1, 32, peak / 4, peak / 2, peak - 1, peak] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match run(&budget, &mut || Ok(())) {
            Ok(output) => {
                assert_eq!(&*output, expected);
                assert_eq!(budget.used(), owned(&output) + 7);
            }
            Err(StorageBackendError::Analysis(AnalysisError::Memory(MemoryError::Limit {
                ..
            }))) => assert!(allowance < peak),
            output => panic!("{output:?}"),
        }
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    for stop in 1..=calls {
        let budget = MemoryBudget::new(1 << 24);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        assert!(
            matches!(
                run(&budget, &mut || {
                    count += 1;
                    if count == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(StorageBackendError::Analysis(AnalysisError::Cancelled))
            ),
            "callback={stop}"
        );
        assert_eq!(budget.used(), 7);
        drop(other);
    }
}

fn graph_bytes(graph: &Vec<(TokenTermKey, TokenOccurrence)>) -> usize {
    graph.capacity() * size_of::<(TokenTermKey, TokenOccurrence)>()
        + graph
            .iter()
            .map(|(key, _)| key.as_bytes().len())
            .sum::<usize>()
}

#[test]
fn query_ownership_preserves_duplicate_terms_holes_and_original_coordinates() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Stop {
            language: "none".into(),
            custom_words: vec!["the".into()],
        }],
        Vec::new(),
    )
    .compile()
    .unwrap();
    let expected: Vec<_> = [("a", 1, 4, 5), ("a", 2, 6, 7), ("b", 4, 12, 13)]
        .map(|(key, position, start, end)| {
            (
                TokenTermKey::from_text(key),
                TokenOccurrence {
                    position,
                    position_length: 1,
                    offsets: Some(TokenOffsets {
                        start_utf8: start,
                        end_utf8: end,
                        start_utf16: start,
                        end_utf16: end,
                    }),
                },
            )
        })
        .into();
    verify(&expected, graph_bytes, |budget, poll| {
        analyze_query_graph_budgeted(&analyzer, "the a a the b the", budget, poll)
    });
    let expected: Vec<_> = ["a", "a", "b"].map(TokenTermKey::from_text).into();
    verify(
        &expected,
        |keys| {
            keys.capacity() * size_of::<TokenTermKey>()
                + keys.iter().map(|key| key.as_bytes().len()).sum::<usize>()
        },
        |budget, poll| analyze_query_terms_budgeted(&analyzer, "the a a the b the", budget, poll),
    );
}

#[test]
fn query_keys_drop_analysis_source_maps_and_keep_mapped_scalar_coordinates() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        Vec::new(),
        vec![CharFilter::HTMLStrip],
    )
    .compile()
    .unwrap();
    let expected = vec![(
        TokenTermKey::from_text("韓&🙂"),
        TokenOccurrence {
            position: 0,
            position_length: 1,
            offsets: Some(TokenOffsets {
                start_utf8: 3,
                end_utf8: 15,
                start_utf16: 3,
                end_utf16: 11,
            }),
        },
    )];
    verify(&expected, graph_bytes, |budget, poll| {
        analyze_query_graph_budgeted(&analyzer, "<i>韓&amp;🙂</i>", budget, poll)
    });
}

#[test]
fn raw_korean_query_graph_retains_only_keys_after_morphology_and_source_are_released() {
    let config = r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"discard","user_dictionary":"🙂a 가 나"},"char_filters":[{"type":"html_strip"}],"token_filters":[]}"#;
    let analyzer = match serde_json::from_str::<Analyzer>(config) {
        Ok(analyzer) => analyzer.compile().unwrap(),
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return;
        }
    };
    let text = "<i>🙂a</i>";
    let stream = analyzer.analyze_tokens(text).unwrap();
    let mut position = -1_i64;
    let expected: Vec<_> = stream
        .tokens()
        .iter()
        .map(|token| {
            position += i64::from(token.position_increment());
            (
                TokenTermKey::from_term(token.term()),
                TokenOccurrence {
                    position: u32::try_from(position).unwrap(),
                    position_length: token.position_length(),
                    offsets: token.offsets().map(source_offsets).transpose().unwrap(),
                },
            )
        })
        .collect();
    assert!(expected.iter().any(|(key, _)| key.as_str().is_none()));
    drop(stream);
    verify(&expected, graph_bytes, |budget, poll| {
        analyze_query_graph_budgeted(&analyzer, text, budget, poll)
    });
}
