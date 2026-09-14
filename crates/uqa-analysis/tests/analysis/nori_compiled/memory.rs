//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::nori::{DictionaryError, NoriLimits, NoriOptions};
use uqa_analysis::{AnalysisResult, AnalyzedText, CompiledAnalyzer};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};

const SOURCE: &str = "<b>３．２천 원 韓國 감싸여 🙂a UQA</b>";

fn analyzers() -> (Analyzer, Arc<CompiledAnalyzer>, KoreanAnalyzer) {
    let options = NoriOptions {
        decompound_mode: DecompoundMode::Mixed,
        discard_punctuation: false,
        ..Default::default()
    };
    let rules = "🙂a 가 나";
    let user = UserDictionary::compile(rules, model(), UserDictionaryLimits::default()).unwrap();
    let native = KoreanAnalyzer::with_filters(
        model().clone(),
        user,
        options,
        &[
            KoreanFilter::PartOfSpeech {
                stop_tags: Some(vec![POSTag::SP]),
            },
            KoreanFilter::Number,
            KoreanFilter::ReadingForm,
            KoreanFilter::SimpleLowercase,
        ],
    )
    .unwrap();
    let analyzer = Analyzer::new(
        Tokenizer::Nori(NoriTokenizerConfig {
            dictionary: exact_dictionary(),
            user_dictionary: Some(rules.into()),
            decompound_mode: options.decompound_mode,
            discard_punctuation: options.discard_punctuation,
            ..Default::default()
        }),
        vec![
            TokenFilter::NoriPartOfSpeech(NoriPOSConfig {
                stop_tags: Some(vec![POSTag::SP]),
            }),
            TokenFilter::NoriNumber(EmptyFilterConfig::default()),
            TokenFilter::NoriReadingForm(EmptyFilterConfig::default()),
            TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
                unicode_profile: exact_dictionary(),
            }),
        ],
        vec![CharFilter::HTMLStrip],
    );
    let compiled = analyzer.compile().unwrap();
    (analyzer, compiled, native)
}

fn run(
    analyzers: &(Analyzer, Arc<CompiledAnalyzer>, KoreanAnalyzer),
    mode: usize,
    budget: &MemoryBudget,
    mut poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<AnalyzedText>> {
    match mode {
        0 => analyzers.0.analyze_tokens_budgeted(SOURCE, budget, poll),
        1 => analyzers.1.analyze_tokens_budgeted(SOURCE, budget, poll),
        _ => {
            let mapped =
                CharFilter::HTMLStrip.filter_with_offsets_budgeted(SOURCE, budget, &mut poll)?;
            analyzers
                .2
                .analyze_mapped_budgeted(&mapped, NoriLimits::default(), budget, &mut poll)
        }
    }
}

#[test]
fn complete_korean_pipelines_retain_lossless_graphs_and_source_owners() {
    let analyzers = analyzers();
    let expected = analyzers.1.analyze_tokens(SOURCE).unwrap();
    assert!(expected.tokens().iter().any(|token| token.term() == "3200"));
    assert!(expected.tokens().iter().any(|token| token.term() == "한국"));
    assert!(expected
        .tokens()
        .iter()
        .any(|token| token.term().as_str().is_none()));
    assert!(expected
        .tokens()
        .iter()
        .any(|token| token.position_length() > 1));
    for mode in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let output = run(&analyzers, mode, &budget, &mut || Ok(())).unwrap();
        assert_eq!(*output, expected, "mode={mode}");
        assert!(budget.used() > output.reserved_bytes());
        let retained = budget.used();
        let output = output.into_shared().unwrap();
        let clone = output.clone();
        drop(output);
        assert!(budget.used() >= retained);
        assert_eq!(clone.final_offsets().utf8.end, SOURCE.len());
        drop(clone);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let output = analyzers
        .2
        .analyze_budgeted(
            "３．２천 원 韓國",
            NoriLimits::default(),
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(budget.used(), output.reserved_bytes());
    assert!(output
        .tokens
        .iter()
        .any(|token| token.term_utf16 == "3200".encode_utf16().collect::<Vec<_>>()));
    drop(output);
    assert_eq!(budget.used(), 0);
    let output = analyzers
        .2
        .analyze_tokens_budgeted("韓國", NoriLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(output.tokens()[0].term(), "한국");
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_korean_pipeline_quota_failures_release_only_their_own_buffers() {
    let analyzers = analyzers();
    for mode in 0..3 {
        let baseline = MemoryBudget::new(1 << 20);
        let expected = run(&analyzers, mode, &baseline, &mut || Ok(())).unwrap();
        let peak = baseline.peak();
        let mut failures = 0;
        for allowance in (0..peak).step_by(271).chain([peak]) {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match run(&analyzers, mode, &budget, &mut || Ok(())) {
                Ok(output) => assert_eq!(*output, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => failures += 1,
                result => panic!("mode={mode}, allowance={allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(failures > 2);
        let exact = MemoryBudget::new(peak);
        let output = run(&analyzers, mode, &exact, &mut || Ok(())).unwrap();
        assert_eq!(*output, *expected);
        drop(output);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn every_korean_pipeline_callback_can_cancel_without_retaining_partial_output() {
    let analyzers = analyzers();
    for mode in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let mut polls = 0;
        let output = run(&analyzers, mode, &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        drop(output);
        assert!(polls > 20);
        for stop in 1..=polls {
            let mut calls = 0;
            let output = run(&analyzers, mode, &budget, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(output, Err(AnalysisError::Cancelled)),
                "mode={mode}, callback={stop}: {output:?}"
            );
            assert_eq!(budget.used(), 7);
        }
        drop(other);
    }
}

#[test]
fn borrowed_native_source_caches_are_not_published_by_failed_analysis() {
    let (_, _, native) = analyzers();
    let budget = MemoryBudget::new(1 << 20);
    let source = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓國</b>", &budget, &mut || Ok(()))
        .unwrap();
    let retained = budget.used();
    let mut polls = 0;
    let output = native
        .analyze_mapped_budgeted(&source, NoriLimits::default(), &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    drop(output);
    assert_eq!(budget.used(), retained);
    for stop in 1..=polls {
        let mut calls = 0;
        let output =
            native.analyze_mapped_budgeted(&source, NoriLimits::default(), &budget, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
        assert!(matches!(output, Err(AnalysisError::Cancelled)));
        assert_eq!(budget.used(), retained);
        assert_eq!(source.as_str(), " 韓國 ");
    }
    drop(source);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_text_normalization_retains_encodings_and_preserves_raw_units() {
    let (_, compiled, native) = analyzers();
    let source = "<b>喜悲哀歡 İ UQA ΣΟΣ 🙂</b>";
    let expected = "<b>喜悲哀歡 i uqa σοσ 🙂</b>";
    for frozen in [false, true] {
        let normalize = |budget: &MemoryBudget,
                         mut poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            if frozen {
                compiled.normalize_budgeted(source, budget, poll)
            } else {
                native.normalize_budgeted(source, NoriLimits::default(), budget, &mut poll)
            }
        };
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let output = normalize(&baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(baseline.used(), output.capacity());
        assert_eq!(output.reserved_bytes(), output.capacity());
        let peak = baseline.peak();
        drop(output);
        assert_eq!(baseline.used(), 0);
        for allowance in 0..=peak {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match normalize(&budget, &mut || Ok(())) {
                Ok(output) => assert_eq!(*output, expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                result => panic!("frozen={frozen}, allowance={allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut calls = 0;
            let output = normalize(&budget, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(output, Err(AnalysisError::Cancelled)));
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
    let raw = [0xd800, u16::from(b'A'), 0xdc00, 0x0130];
    let budget = MemoryBudget::new(size_of_val(&raw));
    let output = native
        .normalize_utf16_budgeted(&raw, NoriLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*output, [0xd800, u16::from(b'a'), 0xdc00, u16::from(b'i')]);
    assert_eq!(budget.used(), size_of_val(&raw));
    assert_eq!(
        output.reserved_bytes(),
        output.capacity() * size_of::<u16>()
    );
    drop(output);
    assert_eq!(budget.used(), 0);
    for limits in [
        NoriLimits {
            max_input_utf16: 2,
            ..Default::default()
        },
        NoriLimits {
            max_output_utf16: 2,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            native.normalize_utf16_budgeted(&raw, limits, &budget, &mut || Ok(())),
            Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
        ));
        assert_eq!(budget.used(), 0);
    }
    let ordinary = Analyzer::default().compile().unwrap();
    assert!(matches!(
        ordinary.normalize_budgeted("x", &budget, || Err(AnalysisError::Cancelled)),
        Err(AnalysisError::NormalizationUnavailable)
    ));
}
