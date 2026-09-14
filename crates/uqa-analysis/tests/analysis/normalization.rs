//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::{json, Value};
use uqa_analysis::{
    AnalysisError, Analyzer, AnalyzerLimits, AnalyzerResources, CharFilter, NormalizationConfig,
    TokenFilter, Tokenizer,
};

#[path = "normalization/memory.rs"]
mod memory;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
#[path = "normalization/profiles.rs"]
mod profiles;

fn keyword() -> Analyzer {
    Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new())
}

#[test]
fn omitted_normalization_preserves_legacy_json_and_fixed_identity() {
    let wire = json!({"tokenizer": {"type": "keyword"}, "token_filters": [], "char_filters": []});
    let config: Analyzer = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(config.normalization, None);
    assert_eq!(serde_json::to_value(&config).unwrap(), wire);
    let compiled = config.compile().unwrap();
    assert_eq!(
        compiled.descriptor().fingerprint().to_string(),
        "1635f01226f122fecdf2f5d2a9b099b28c81c1d1933a49128b52a46c2ec263df"
    );
    assert!(matches!(
        compiled.normalize("ＵＱＡ"),
        Err(AnalysisError::NormalizationUnavailable)
    ));
    let disabled = config
        .with_normalization(NormalizationConfig::Unavailable)
        .compile()
        .unwrap();
    assert_ne!(
        disabled.descriptor().fingerprint(),
        compiled.descriptor().fingerprint()
    );
    assert!(matches!(
        disabled.normalize("x"),
        Err(AnalysisError::NormalizationUnavailable)
    ));
}

#[test]
fn explicit_width_normalization_is_independent_of_analysis_stages_and_restores() {
    let config = Analyzer::new(
        Tokenizer::Keyword,
        vec![TokenFilter::Lowercase],
        vec![CharFilter::PatternReplace {
            pattern: ".+".into(),
            replacement: "REPLACED".into(),
        }],
    )
    .with_normalization(NormalizationConfig::CJKWidth);
    let compiled = config.compile().unwrap();
    assert_eq!(compiled.analyze("ＵＱＡ ｶﾞ ①").unwrap(), ["replaced"]);
    assert_eq!(compiled.normalize("ＵＱＡ ｶﾞ ①").unwrap(), "UQA ガ ①");
    let wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
    assert_eq!(
        wire["descriptor"]["pipeline"]["normalization"],
        json!({"type": "cjk_width"})
    );
    assert_eq!(
        wire["descriptor"]["runtime_profiles"]["normalization_unicode"],
        json!(unicode_normalization::UNICODE_VERSION)
    );
    let restored = AnalyzerResources::new(AnalyzerLimits::default())
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    for input in ["", "ＵＱＡ ｶﾞ ①", "İ ΟΣ 𐐀", "\0\r\n"] {
        assert_eq!(
            restored.normalize(input).unwrap(),
            compiled.normalize(input).unwrap()
        );
    }
}

#[test]
fn normalization_rejects_unknown_properties_and_disabled_profile_providers() {
    for value in [
        json!({"type": "unavailable", "profile": null}),
        json!({"type": "cjk_width", "extra": true}),
        json!({"type": "unknown"}),
        json!({"type": "unicode_simple_lowercase"}),
        json!({"type": "unicode_simple_lowercase", "profile": {"provider": "unknown", "dictionary": "x"}}),
    ] {
        assert!(
            serde_json::from_value::<NormalizationConfig>(value.clone()).is_err(),
            "{value}"
        );
    }
    for (provider, enabled) in [
        ("nori", cfg!(feature = "nori")),
        ("kuromoji", cfg!(feature = "kuromoji")),
    ] {
        let value = json!({"type": "unicode_simple_lowercase", "profile": {"provider": provider, "dictionary": "lucene-10.5.1"}});
        assert_eq!(
            serde_json::from_value::<NormalizationConfig>(value.clone()).is_ok(),
            enabled
        );
        let mut unknown = value.clone();
        unknown["profile"]["extra"] = json!(true);
        assert!(serde_json::from_value::<NormalizationConfig>(unknown).is_err());
        let mut missing = value;
        missing["profile"]
            .as_object_mut()
            .unwrap()
            .remove("dictionary");
        assert!(serde_json::from_value::<NormalizationConfig>(missing).is_err());
    }
}

#[test]
fn normalization_stage_limits_apply_before_resource_resolution_and_publication() {
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_stages: 1,
        ..Default::default()
    });
    assert!(resources.compile(&keyword()).is_ok());
    assert!(matches!(
        resources.compile(&keyword().with_normalization(NormalizationConfig::CJKWidth)),
        Err(AnalysisError::ResourceLimit {
            resource: "analyzer stages",
            required: 2,
            limit: 1
        })
    ));
    assert_eq!(resources.cache_stats().analyzers, 1);
}
