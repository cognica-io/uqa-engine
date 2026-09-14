//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::json;
use uqa_analysis::TokenFilter;

const COMPONENTS: [&str; 6] = [
    "kuromoji_baseform",
    "kuromoji_stemmer",
    "kuromoji_hiragana_uppercase",
    "kuromoji_katakana_uppercase",
    "kuromoji_readingform",
    "kuromoji_number",
];

#[test]
fn japanese_filter_components_require_the_japanese_feature() {
    for component in COMPONENTS {
        assert_eq!(
            serde_json::from_value::<TokenFilter>(json!({"type": component})).is_ok(),
            cfg!(feature = "kuromoji")
        );
    }
}

#[cfg(feature = "kuromoji")]
mod enabled {
    use super::*;
    use std::sync::Arc;
    use uqa_analysis::kuromoji::{DictionaryRequest, KuromojiResources, ResourceLimits};
    use uqa_analysis::{
        AnalysisError, Analyzer, AnalyzerLimits, AnalyzerResources, CharFilter, TokenLengthPolicy,
        Tokenizer,
    };
    use uqa_core::memory::MemoryBudget;

    fn resources() -> AnalyzerResources {
        AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(KuromojiResources::with_resolver(
                Arc::new(|_: &DictionaryRequest| {
                    panic!("a resource-independent filter must not resolve a dictionary")
                }),
                ResourceLimits::default(),
            ))
            .build()
    }

    #[test]
    fn pure_japanese_stages_compile_restore_and_execute_without_dictionary_ownership() {
        for (component, input, expected) in [
            ("kuromoji_baseform", "UQA", "UQA"),
            ("kuromoji_stemmer", "シャワー", "シャワ"),
            ("kuromoji_hiragana_uppercase", "きゃ", "きや"),
            ("kuromoji_katakana_uppercase", "キャ", "キヤ"),
            ("kuromoji_readingform", "シャワー", "シャワー"),
            ("kuromoji_number", "二百三", "203"),
        ] {
            let filter: TokenFilter = serde_json::from_value(json!({"type": component})).unwrap();
            filter.validate().unwrap();
            assert_eq!(filter.filter(vec![input.into()]).unwrap(), [expected]);
            let config = Analyzer::new(Tokenizer::Keyword, vec![filter], Vec::new());
            assert!(config.uses_japanese_stages());
            assert!(!config.uses_korean_stages());
            let owner = resources();
            let compiled = owner.compile(&config).unwrap();
            assert_eq!(
                compiled.descriptor().length_policy(),
                TokenLengthPolicy::DiscountOverlaps
            );
            assert_eq!(compiled.analyze(input).unwrap(), [expected]);
            let restored = resources()
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap();
            assert_eq!(
                restored.analyze_tokens(input).unwrap(),
                config.analyze_tokens(input).unwrap()
            );
            assert_eq!(owner.kuromoji_resources().cache_stats().dictionaries, 0);
            assert!(!compiled.descriptor().canonical_json().contains("sha256:"));
            assert!(matches!(
                compiled.normalize(input),
                Err(AnalysisError::NormalizationUnavailable)
            ));
        }
    }

    #[test]
    fn strict_filter_configuration_preserves_defaults_and_rejects_unused_resources() {
        for component in COMPONENTS {
            for extra in ["dictionary", "unknown"] {
                assert!(serde_json::from_value::<TokenFilter>(
                    json!({"type": component, extra: "unavailable"})
                )
                .is_err());
            }
        }
        for (component, property, expected) in [
            ("kuromoji_stemmer", "minimum_length", json!(4)),
            ("kuromoji_readingform", "use_romaji", json!(false)),
        ] {
            let config: Analyzer =
                serde_json::from_value(json!({"token_filters": [{"type": component}]})).unwrap();
            let compiled = resources().compile(&config).unwrap();
            let resolved =
                serde_json::to_value(compiled.descriptor().configuration().unwrap()).unwrap();
            assert_eq!(resolved["token_filters"][0][property], expected);
        }
        for value in [0, -1, i32::MIN] {
            let config: Analyzer = serde_json::from_value(
                json!({"token_filters": [{"type": "kuromoji_stemmer", "minimum_length": value}]}),
            )
            .unwrap();
            let owner = resources();
            assert!(owner.compile(&config).is_err());
            assert_eq!(owner.cache_stats().analyzers, 0);
        }
    }

    #[test]
    fn pure_japanese_filter_chains_keep_source_and_release_every_interrupted_run() {
        let filters = COMPONENTS
            .iter()
            .map(|component| serde_json::from_value(json!({"type": component})).unwrap())
            .collect();
        let config = Analyzer::new(
            Tokenizer::Whitespace,
            filters,
            vec![CharFilter::HTMLStrip, CharFilter::CJKWidth],
        );
        let compiled = resources().compile(&config).unwrap();
        let input = "<b>二百三 きゃ キャ シャワー ＵＱＡ</b>";
        let budget = MemoryBudget::new(usize::MAX);
        let mut polls = 0;
        let expected = compiled
            .analyze_tokens_budgeted(input, &budget, || {
                polls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(*expected, config.analyze_tokens(input).unwrap());
        assert_eq!(
            &input[expected.tokens()[0].offsets().unwrap().utf8.clone()],
            "二百三"
        );
        for cutoff in 1..=polls {
            let budget = MemoryBudget::new(usize::MAX);
            let mut calls = 0;
            assert!(matches!(
                compiled.analyze_tokens_budgeted(input, &budget, || {
                    calls += 1;
                    if calls == cutoff {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(AnalysisError::Cancelled)
            ));
            assert_eq!(budget.used(), 0);
        }
        let peak = budget.peak();
        let mut failures = 0;
        for limit in (0..=peak).step_by((peak / 17).max(1)) {
            let budget = MemoryBudget::new(limit + 7);
            let held = budget.reserve(7).unwrap();
            match compiled.analyze_tokens_budgeted(input, &budget, || Ok(())) {
                Ok(output) => assert_eq!(*output, *expected),
                Err(AnalysisError::Memory(_)) => failures += 1,
                error => panic!("unexpected outcome: {error:?}"),
            }
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        assert!(failures > 0);
        drop(expected);
        assert_eq!(budget.used(), 0);
    }
}
