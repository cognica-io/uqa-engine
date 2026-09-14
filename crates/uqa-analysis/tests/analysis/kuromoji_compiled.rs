//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::json;
use uqa_analysis::{get_analyzer, is_builtin_analyzer, TokenFilter};

#[test]
fn japanese_builtins_and_profiled_components_follow_feature_selection() {
    for component in [
        "kuromoji_part_of_speech",
        "kuromoji_stop",
        "kuromoji_completion",
    ] {
        assert_eq!(
            serde_json::from_value::<TokenFilter>(json!({"type": component})).is_ok(),
            cfg!(feature = "kuromoji")
        );
    }
    for name in ["kuromoji", "kuromoji_completion"] {
        assert_eq!(is_builtin_analyzer(name), cfg!(feature = "kuromoji"));
        assert_eq!(get_analyzer(name).is_ok(), cfg!(feature = "kuromoji"));
        #[cfg(feature = "kuromoji")]
        {
            use uqa_analysis::registry::RegistryError;
            assert!(matches!(
                uqa_analysis::register_analyzer(name, uqa_analysis::Analyzer::default()),
                Err(RegistryError::OverwriteBuiltin(_))
            ));
            assert!(matches!(
                uqa_analysis::drop_analyzer(name),
                Err(RegistryError::DropBuiltin(_))
            ));
        }
    }
}

#[cfg(feature = "kuromoji")]
mod enabled {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use uqa_analysis::kuromoji::{DictionaryRequest, KuromojiResources, ResourceLimits};
    use uqa_analysis::{AnalysisError, Analyzer, AnalyzerLimits, AnalyzerResources, Tokenizer};

    fn without_resources() -> AnalyzerResources {
        AnalyzerResources::builder(AnalyzerLimits::default())
            .kuromoji_resources(KuromojiResources::with_resolver(
                Arc::new(|_: &DictionaryRequest| panic!("unexpected dictionary lookup")),
                ResourceLimits::default(),
            ))
            .build()
    }

    fn pipeline(filter: Value) -> Analyzer {
        Analyzer::new(
            Tokenizer::Whitespace,
            vec![serde_json::from_value(filter).unwrap()],
            Vec::new(),
        )
    }

    fn rehash(wire: &mut Value) -> String {
        let mut hash = Sha256::new();
        hash.update(b"UQA analyzer descriptor\0");
        hash.update(serde_json::to_vec(&wire["descriptor"]).unwrap());
        wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
        wire.to_string()
    }

    #[test]
    fn explicit_sets_are_canonical_without_unused_dictionary_dependencies() {
        for config in [
            json!({"type": "kuromoji_part_of_speech", "stop_tags": ["名詞", "動詞", "名詞"]}),
            json!({"type": "kuromoji_stop", "words": ["B", "A", "B"], "ignore_case": false}),
        ] {
            let mut original = pipeline(config);
            let owner = without_resources();
            let compiled = owner.compile(&original).unwrap();
            let resolved = compiled.descriptor().configuration().unwrap();
            let wire = serde_json::to_value(&resolved).unwrap();
            assert_eq!(wire["token_filters"][0]["dictionary"], Value::Null);
            assert!(!compiled.descriptor().canonical_json().contains("sha256:"));
            let output = compiled.analyze_tokens("A B uqa").unwrap();
            assert_eq!(
                without_resources()
                    .restore_json(compiled.descriptor().canonical_json())
                    .unwrap()
                    .analyze_tokens("A B uqa")
                    .unwrap(),
                output
            );
            match &mut original.token_filters[0] {
                TokenFilter::KuromojiPartOfSpeech(config) => {
                    config.stop_tags = Some(vec!["動詞".into(), "名詞".into()]);
                }
                TokenFilter::KuromojiStop(config) => {
                    config.words = Some(vec!["A".into(), "B".into()]);
                }
                _ => unreachable!(),
            }
            assert_eq!(
                owner.compile(&original).unwrap().descriptor().fingerprint(),
                compiled.descriptor().fingerprint()
            );
            let mut invalid = serde_json::to_value(&original).unwrap();
            invalid["token_filters"][0]["dictionary"] = json!("unused");
            let invalid: Analyzer = serde_json::from_value(invalid).unwrap();
            assert!(matches!(
                without_resources().compile(&invalid),
                Err(AnalysisError::Descriptor(
                    "explicit Japanese stop sets must not specify an unused dictionary"
                ))
            ));
            assert_eq!(owner.kuromoji_resources().cache_stats().dictionaries, 0);
        }
        let empty = pipeline(json!({"type":"kuromoji_stop", "words":[], "ignore_case":false}));
        assert_eq!(
            without_resources()
                .compile(&empty)
                .unwrap()
                .analyze("A B")
                .unwrap(),
            ["A", "B"]
        );
    }

    #[test]
    fn stop_case_policy_is_explicit_and_original_word_case_survives_snapshots() {
        for ignore_case in [false, true] {
            let config = pipeline(
                json!({"type":"kuromoji_stop", "words":["UQA", "İ"], "ignore_case":ignore_case}),
            );
            let compiled = config.compile().unwrap();
            let resolved =
                serde_json::to_value(compiled.descriptor().configuration().unwrap()).unwrap();
            assert_eq!(resolved["token_filters"][0]["words"], json!(["UQA", "İ"]));
            let dictionary = &resolved["token_filters"][0]["dictionary"];
            assert_eq!(dictionary.is_string(), ignore_case);
            if ignore_case {
                assert_eq!(
                    dictionary,
                    &json!(format!("sha256:{}", uqa_kuromoji_data::BUNDLE_SHA256))
                );
            }
            let expected = if ignore_case {
                vec!["keep"]
            } else {
                vec!["uqa", "i", "keep"]
            };
            assert_eq!(compiled.analyze("UQA uqa İ i keep").unwrap(), expected);
            assert_eq!(config.analyze("UQA uqa İ i keep").unwrap(), expected);
            assert_eq!(
                AnalyzerResources::new(AnalyzerLimits::default())
                    .restore_json(compiled.descriptor().canonical_json())
                    .unwrap()
                    .analyze("UQA uqa İ i keep")
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn restoration_rejects_unresolved_sets_profiles_and_noncanonical_lists() {
        for (filter, property) in [
            (json!({"type":"kuromoji_part_of_speech"}), "stop_tags"),
            (json!({"type":"kuromoji_stop"}), "words"),
            (json!({"type":"kuromoji_completion"}), "dictionary"),
        ] {
            let compiled = pipeline(filter).compile().unwrap();
            let original: Value =
                serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
            for replacement in [
                Value::Null,
                if property == "dictionary" {
                    json!("lucene-10.5.1")
                } else {
                    json!(["z", "a", "z"])
                },
            ] {
                let mut wire = original.clone();
                wire["descriptor"]["pipeline"]["token_filters"][0][property] = replacement;
                let owner = AnalyzerResources::new(AnalyzerLimits::default());
                assert!(owner.restore_json(&rehash(&mut wire)).is_err());
                assert_eq!(owner.cache_stats().analyzers, 0);
            }
            let mut wire = original;
            wire["descriptor"]["pipeline"]["token_filters"][0]["dictionary"] = json!("unresolved");
            assert!(AnalyzerResources::new(AnalyzerLimits::default())
                .restore_json(&rehash(&mut wire))
                .is_err());
        }
        let config = pipeline(json!({"type":"kuromoji_stop", "words":[]}))
            .compile()
            .unwrap();
        let mut wire: Value = serde_json::from_str(config.descriptor().canonical_json()).unwrap();
        wire["descriptor"]["pipeline"]["token_filters"][0]["dictionary"] = Value::Null;
        assert!(AnalyzerResources::new(AnalyzerLimits::default())
            .restore_json(&rehash(&mut wire))
            .is_err());
    }

    #[test]
    fn strict_profiled_filters_fail_before_publication_for_invalid_fields_resources_and_limits() {
        for component in [
            "kuromoji_part_of_speech",
            "kuromoji_stop",
            "kuromoji_completion",
        ] {
            assert!(serde_json::from_value::<TokenFilter>(
                json!({"type":component,"unknown":true})
            )
            .is_err());
            let config = pipeline(json!({"type":component,"dictionary":"missing-test-dictionary"}));
            let owner = AnalyzerResources::new(AnalyzerLimits::default());
            assert!(owner.compile(&config).is_err());
            assert_eq!(owner.cache_stats().analyzers, 0);
        }
        assert!(serde_json::from_value::<TokenFilter>(
            json!({"type":"kuromoji_completion","mode":"other"})
        )
        .is_err());
        let config = pipeline(
            json!({"type":"kuromoji_stop", "words": vec![""; 65537], "ignore_case": false}),
        );
        let owner = without_resources();
        assert!(owner.compile(&config).is_err());
        assert_eq!(owner.cache_stats().analyzers, 0);
    }

    #[test]
    fn compiled_japanese_builtins_preserve_source_and_unwind_every_cancelled_run() {
        use uqa_core::memory::MemoryBudget;
        for name in ["kuromoji", "kuromoji_completion"] {
            let mut config = get_analyzer(name).unwrap();
            config
                .char_filters
                .insert(0, uqa_analysis::CharFilter::HTMLStrip);
            let compiled = config.compile().unwrap();
            let input = "<b>東京タワーで走りました ＵＱＡ ｻｯk</b>";
            let budget = MemoryBudget::new(usize::MAX);
            let mut polls = 0;
            let expected = compiled
                .analyze_tokens_budgeted(input, &budget, || {
                    polls += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(*expected, config.analyze_tokens(input).unwrap());
            let token = expected
                .tokens()
                .iter()
                .find(|token| token.term() == "uqa")
                .unwrap();
            assert_eq!(&input[token.offsets().unwrap().utf8.clone()], "ＵＱＡ");
            for cutoff in 1..=polls {
                let budget = MemoryBudget::new(usize::MAX);
                let mut count = 0;
                assert!(matches!(
                    compiled.analyze_tokens_budgeted(input, &budget, || {
                        count += 1;
                        if count == cutoff {
                            Err(AnalysisError::Cancelled)
                        } else {
                            Ok(())
                        }
                    }),
                    Err(AnalysisError::Cancelled)
                ));
                assert_eq!(budget.used(), 0, "{name} callback {cutoff}");
            }
            let peak = budget.peak();
            let mut failures = 0;
            for limit in (0..=peak).step_by((peak / 17).max(1)) {
                let budget = MemoryBudget::new(limit + 7);
                let held = budget.reserve(7).unwrap();
                match compiled.analyze_tokens_budgeted(input, &budget, || Ok(())) {
                    Ok(output) => assert_eq!(*output, *expected),
                    Err(AnalysisError::Memory(_)) => failures += 1,
                    result => panic!("unexpected {name} budget result: {result:?}"),
                }
                assert_eq!(budget.used(), 7);
                drop(held);
            }
            assert!(failures > 0);
            drop(expected);
            assert_eq!(budget.used(), 0);
            assert_eq!(
                compiled.normalize("ＵＱＡ").unwrap(),
                if name == "kuromoji" { "uqa" } else { "UQA" }
            );
        }
    }
}
