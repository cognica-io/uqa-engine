//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, sync::Arc};

use serde_json::{json, Value};
use uqa_analysis::{
    Analyzer, AnalyzerDescriptor, AnalyzerLimits, AnalyzerResources, CharFilter, TokenFilter,
    TokenLengthPolicy, Tokenizer,
};

#[path = "descriptor/cache.rs"]
mod cache;
#[path = "descriptor/validation.rs"]
mod validation;

fn resolve(analyzer: &Analyzer) -> Arc<AnalyzerDescriptor> {
    AnalyzerDescriptor::resolve(
        analyzer,
        TokenLengthPolicy::EmittedTokens,
        AnalyzerLimits::default(),
    )
    .unwrap()
}

fn keyword() -> Analyzer {
    Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new())
}

#[test]
fn canonical_descriptor_has_portable_fixed_identity_and_explicit_defaults() {
    let descriptor = resolve(&keyword());
    assert_eq!(
        descriptor.fingerprint().to_string(),
        "1635f01226f122fecdf2f5d2a9b099b28c81c1d1933a49128b52a46c2ec263df"
    );
    let json = descriptor.canonical_json();
    assert_eq!(serde_json::to_string(descriptor.as_ref()).unwrap(), json);
    let reordered =
        serde_json::to_string_pretty(&serde_json::from_str::<Value>(json).unwrap()).unwrap();
    let restored = AnalyzerDescriptor::from_json(&reordered, AnalyzerLimits::default()).unwrap();
    assert_eq!(restored.canonical_json(), json);
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = resources.restore(restored).unwrap();
    assert_eq!(
        compiled.analyze("喜悲哀歡 İ UQA").unwrap(),
        ["喜悲哀歡 İ UQA"]
    );
    assert!(Arc::ptr_eq(
        &compiled,
        &resources.compile(&keyword()).unwrap()
    ));
    let overlap = resources
        .compile_with_length_policy(&keyword(), TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    assert_ne!(overlap.descriptor().fingerprint(), descriptor.fingerprint());
    assert_eq!(
        overlap.descriptor().length_policy(),
        TokenLengthPolicy::DiscountOverlaps
    );
}

#[test]
fn contextual_lowercase_preserves_stored_unicode_16_analyzer_identities() {
    for (tokenizer, fingerprint) in [
        (
            Tokenizer::Keyword,
            "7c1c9abfe64d2781e713010a918aaedac65574405508416abba74de1e97f5c3d",
        ),
        (
            Tokenizer::Whitespace,
            "a586595281c69fc6600626b208ed1c3d6afe775df10af59415696830530b8b3d",
        ),
        (
            Tokenizer::Standard,
            "eb7b097ddd7ee7fc9870433a155108c52ec652d002284b70c1142f37e8bea7fb",
        ),
    ] {
        let config = Analyzer::new(tokenizer, vec![TokenFilter::Lowercase], Vec::new());
        let descriptor = resolve(&config);
        assert_eq!(descriptor.fingerprint().to_string(), fingerprint);
        let restored = AnalyzerResources::new(AnalyzerLimits::default())
            .restore_json(descriptor.canonical_json())
            .unwrap();
        assert_eq!(
            restored.descriptor().canonical_json(),
            descriptor.canonical_json()
        );
        assert_eq!(
            restored.analyze("ΟΣ ΟΣΑ İ").unwrap().join(" "),
            "ος οσα i\u{307}"
        );
    }
}

#[test]
fn canonical_stop_snapshots_merge_builtins_and_preserve_ordered_synonym_multiplicity() {
    let mut config = keyword();
    config.char_filters.push(CharFilter::HTMLStrip);
    config.token_filters = vec![
        TokenFilter::Stop {
            language: "english".into(),
            custom_words: vec!["custom".into(), "the".into(), "custom".into()],
        },
        TokenFilter::ASCIIFolding,
        TokenFilter::Synonym {
            synonyms: BTreeMap::from([("x".into(), vec!["z".into(), "z".into(), "y".into()])]),
            synonyms_path: None,
        },
    ];
    let initial = serde_json::to_value(&config).unwrap();
    let descriptor = resolve(&config);
    assert_eq!(serde_json::to_value(&config).unwrap(), initial);
    let data: Value = serde_json::from_str(descriptor.canonical_json()).unwrap();
    let stop = &data["descriptor"]["pipeline"]["token_filters"][0];
    assert_eq!(stop["language"], "");
    let words: Vec<String> = serde_json::from_value(stop["custom_words"].clone()).unwrap();
    assert_eq!(words.iter().filter(|word| *word == "the").count(), 1);
    assert_eq!(words.iter().filter(|word| *word == "custom").count(), 1);
    config.token_filters[0] = TokenFilter::Stop {
        language: "unknown".into(),
        custom_words: words.into_iter().rev().collect(),
    };
    assert_eq!(resolve(&config).fingerprint(), descriptor.fingerprint());
    let mut alias = initial;
    alias["char_filters"][0]["type"] = json!("h_t_m_l_strip");
    alias["token_filters"][1]["type"] = json!("a_s_c_i_i_folding");
    assert_eq!(
        resolve(&serde_json::from_value(alias).unwrap()).fingerprint(),
        descriptor.fingerprint()
    );
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    assert_eq!(
        resources
            .restore_json(descriptor.canonical_json())
            .unwrap()
            .analyze("x")
            .unwrap(),
        ["x", "z", "z", "y"]
    );
    let TokenFilter::Synonym { synonyms, .. } = &mut config.token_filters[2] else {
        unreachable!()
    };
    synonyms.get_mut("x").unwrap().swap(1, 2);
    assert_ne!(resolve(&config).fingerprint(), descriptor.fingerprint());
}

#[test]
fn file_snapshots_restore_without_paths_and_different_files_can_share_resolved_revisions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.txt");
    let other = directory.path().join("other.txt");
    std::fs::write(&path, "cat => feline\n").unwrap();
    std::fs::write(&other, "# different source\ncat => feline\n").unwrap();
    let mut config = keyword();
    config.token_filters.push(TokenFilter::Synonym {
        synonyms: BTreeMap::new(),
        synonyms_path: Some(path.clone()),
    });
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = resources.compile(&config).unwrap();
    let json = compiled.descriptor().canonical_json().to_owned();
    assert!(!json.contains(directory.path().to_str().unwrap()));
    let TokenFilter::Synonym { synonyms_path, .. } = &mut config.token_filters[0] else {
        unreachable!()
    };
    *synonyms_path = Some(other.clone());
    assert!(Arc::ptr_eq(&compiled, &resources.compile(&config).unwrap()));
    std::fs::write(&other, "cat => animal\n").unwrap();
    let changed = resources.compile(&config).unwrap();
    assert_ne!(
        changed.descriptor().fingerprint(),
        compiled.descriptor().fingerprint()
    );
    std::fs::remove_dir_all(directory.path()).unwrap();
    let reopened = AnalyzerResources::new(AnalyzerLimits::default())
        .restore_json(&json)
        .unwrap();
    assert!(!Arc::ptr_eq(&compiled, &reopened));
    assert_eq!(reopened.analyze("cat").unwrap(), ["cat", "feline"]);
    assert_eq!(changed.analyze("cat").unwrap(), ["cat", "animal"]);
    assert!(resources.compile(&config).is_err());
}

#[test]
fn profiles_track_only_the_character_tables_used_by_the_pipeline() {
    let profile = |config: &Analyzer| -> Value {
        let wire: Value = serde_json::from_str(resolve(config).canonical_json()).unwrap();
        wire["descriptor"]["runtime_profiles"].clone()
    };
    let empty = profile(&keyword());
    assert_eq!(
        empty,
        json!({"expressions": [], "rust_unicode": null, "normalization_unicode": null})
    );
    let mut config = keyword();
    config.token_filters.push(TokenFilter::Lowercase);
    assert_eq!(
        profile(&config)["rust_unicode"],
        json!(char::UNICODE_VERSION)
    );
    config.token_filters = vec![TokenFilter::ASCIIFolding];
    assert_eq!(
        profile(&config)["normalization_unicode"],
        json!(unicode_normalization::UNICODE_VERSION)
    );
    for tokenizer in [
        Tokenizer::Whitespace,
        Tokenizer::NGram {
            min_gram: 1,
            max_gram: 2,
        },
    ] {
        config = Analyzer::new(tokenizer, Vec::new(), Vec::new());
        assert_eq!(
            profile(&config)["rust_unicode"],
            json!(char::UNICODE_VERSION)
        );
    }
    config = Analyzer::new(Tokenizer::Standard, Vec::new(), vec![CharFilter::HTMLStrip]);
    let standard = profile(&config);
    config.tokenizer = Tokenizer::Pattern {
        pattern: "\\w+".into(),
    };
    assert_eq!(profile(&config), standard);
    config.tokenizer = Tokenizer::Letter;
    assert_ne!(
        profile(&config)["expressions"][1],
        standard["expressions"][1]
    );
    assert_eq!(
        profile(&config)["expressions"][0],
        standard["expressions"][0]
    );
    assert!(profile(&config)["rust_unicode"].is_null());
}
