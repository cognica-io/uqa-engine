//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::json;
use uqa_analysis::TokenFilter;

#[test]
fn lowercase_profile_providers_require_their_declared_features() {
    for (provider, enabled) in [
        ("nori", cfg!(feature = "nori")),
        ("kuromoji", cfg!(feature = "kuromoji")),
    ] {
        let value = json!({"type": "unicode_simple_lowercase", "unicode_profile": {
            "provider": provider, "dictionary": "lucene-10.5.1"
        }});
        assert_eq!(
            serde_json::from_value::<TokenFilter>(value).is_ok(),
            enabled
        );
    }
    assert_eq!(
        serde_json::from_value::<TokenFilter>(json!({"type": "unicode_simple_lowercase"})).is_ok(),
        cfg!(any(feature = "nori", feature = "kuromoji"))
    );
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod profiles {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use uqa_analysis::{
        AnalysisError, Analyzer, AnalyzerLimits, AnalyzerResources, SimpleLowercaseConfig,
        TokenLengthPolicy, Tokenizer, UnicodeProfile, UnicodeProfileSource,
    };

    fn config(source: UnicodeProfileSource) -> Analyzer {
        Analyzer::new(
            Tokenizer::Keyword,
            vec![TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
                unicode_profile: source,
            })],
            Vec::new(),
        )
    }

    #[test]
    fn original_string_defaults_round_trip_and_never_select_another_provider() {
        for wire in [
            json!({"type": "unicode_simple_lowercase"}),
            json!({"type": "unicode_simple_lowercase", "unicode_profile": "jdk21"}),
        ] {
            let filter: TokenFilter = serde_json::from_value(wire).unwrap();
            assert_eq!(
                serde_json::to_value(&filter).unwrap(),
                json!({
                    "type": "unicode_simple_lowercase", "unicode_profile": "jdk21"
                })
            );
            assert_eq!(filter.validate().is_ok(), cfg!(feature = "nori"));
        }
        let config = config(UnicodeProfileSource::default());
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        let result = resources.compile(&config);
        #[cfg(not(feature = "nori"))]
        {
            assert!(matches!(
                result,
                Err(AnalysisError::Descriptor(
                    "string Unicode profiles require the nori feature"
                ))
            ));
            assert_eq!(resources.cache_stats().analyzers, 0);
            assert!(config.analyze("UQA").is_err());
        }
        #[cfg(feature = "nori")]
        {
            let compiled = result.unwrap();
            let wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
            assert_eq!(
                wire["descriptor"]["pipeline"]["token_filters"][0],
                json!({
                    "type": "unicode_simple_lowercase", "unicode_profile": format!("sha256:{}", uqa_nori_data::BUNDLE_SHA256)
                })
            );
            assert!(!compiled
                .descriptor()
                .canonical_json()
                .contains("\"provider\""));
            assert_eq!(compiled.analyze("İ ΟΣ 𐐀").unwrap(), ["i οσ 𐐨"]);
            assert!(config.uses_korean_stages());
            assert!(!config.uses_japanese_stages());
        }
    }

    #[test]
    fn profile_objects_are_strict_and_do_not_fall_back_to_legacy_defaults() {
        for profile in [
            Value::Null,
            json!(true),
            json!(1),
            json!([]),
            json!({}),
            json!({"provider": "unknown", "dictionary": "lucene-10.5.1"}),
            json!({"provider": "nori"}),
            json!({"provider": "kuromoji"}),
            json!({"provider": "nori", "dictionary": "lucene-10.5.1", "unknown": true}),
            json!({"provider": "kuromoji", "dictionary": "lucene-10.5.1", "unknown": true}),
        ] {
            assert!(serde_json::from_value::<TokenFilter>(json!({
                "type": "unicode_simple_lowercase", "unicode_profile": profile
            }))
            .is_err());
        }
        assert!(serde_json::from_value::<TokenFilter>(json!({
            "type": "unicode_simple_lowercase", "unknown": true
        }))
        .is_err());
    }

    #[cfg(not(feature = "nori"))]
    #[test]
    fn japanese_only_restoration_rejects_string_profiles_even_with_a_valid_fingerprint() {
        let compiled = config(
            UnicodeProfile::Kuromoji {
                dictionary: "lucene-10.5.1".into(),
            }
            .into(),
        )
        .compile()
        .unwrap();
        let mut wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
        wire["descriptor"]["pipeline"]["token_filters"][0]["unicode_profile"] =
            json!(format!("sha256:{}", uqa_kuromoji_data::BUNDLE_SHA256));
        let mut hash = Sha256::new();
        hash.update(b"UQA analyzer descriptor\0");
        hash.update(serde_json::to_vec(&wire["descriptor"]).unwrap());
        wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        assert!(matches!(
            resources.restore_json(&wire.to_string()),
            Err(AnalysisError::Descriptor(
                "string Unicode profiles require the nori feature"
            ))
        ));
        assert_eq!(resources.cache_stats().analyzers, 0);
    }

    #[test]
    fn explicit_lowercase_profiles_freeze_and_restore_independently_of_the_tokenizer() {
        for (profile, provider, hash) in [
            #[cfg(feature = "nori")]
            (
                UnicodeProfile::Nori {
                    dictionary: "lucene-10.5.1".into(),
                },
                "nori",
                uqa_nori_data::BUNDLE_SHA256,
            ),
            #[cfg(feature = "kuromoji")]
            (
                UnicodeProfile::Kuromoji {
                    dictionary: "lucene-10.5.1".into(),
                },
                "kuromoji",
                uqa_kuromoji_data::BUNDLE_SHA256,
            ),
        ] {
            let config = config(profile.into());
            let original = serde_json::to_value(&config).unwrap();
            assert_eq!(config.uses_korean_stages(), provider == "nori");
            assert_eq!(config.uses_japanese_stages(), provider == "kuromoji");
            let compiled = config.compile().unwrap();
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
            assert_eq!(
                compiled.descriptor().length_policy(),
                TokenLengthPolicy::DiscountOverlaps
            );
            let wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
            assert_eq!(
                wire["descriptor"]["pipeline"]["token_filters"][0]["unicode_profile"],
                json!({
                    "provider": provider, "dictionary": format!("sha256:{hash}")
                })
            );
            assert_eq!(
                wire["descriptor"]["runtime_profiles"]["rust_unicode"],
                Value::Null
            );
            assert_eq!(
                wire["descriptor"]["runtime_profiles"]["normalization_unicode"],
                Value::Null
            );
            let restored = AnalyzerResources::new(AnalyzerLimits::default())
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap();
            let text = "ＵＱＡ İ ΟΣ 𐐀";
            let expected = ["ｕｑａ i οσ 𐐨"];
            assert_eq!(
                config.token_filters[0].filter(vec![text.into()]).unwrap(),
                expected
            );
            assert_eq!(config.analyze(text).unwrap(), expected);
            assert_eq!(compiled.analyze(text).unwrap(), expected);
            assert_eq!(
                restored.analyze_tokens(text).unwrap(),
                compiled.analyze_tokens(text).unwrap()
            );
            assert!(matches!(
                restored.normalize(text),
                Err(AnalysisError::NormalizationUnavailable)
            ));
            for replacement in [
                json!("lucene-10.5.1"),
                json!(format!("sha256:{}", hash.to_uppercase())),
                json!(format!("sha256:{}", "0".repeat(64))),
            ] {
                let mut wire = wire.clone();
                wire["descriptor"]["pipeline"]["token_filters"][0]["unicode_profile"]
                    ["dictionary"] = replacement;
                let mut hash = Sha256::new();
                hash.update(b"UQA analyzer descriptor\0");
                hash.update(serde_json::to_vec(&wire["descriptor"]).unwrap());
                wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
                let resources = AnalyzerResources::new(AnalyzerLimits::default());
                assert!(resources.restore_json(&wire.to_string()).is_err());
                assert_eq!(resources.cache_stats().analyzers, 0);
            }
        }
    }
}
