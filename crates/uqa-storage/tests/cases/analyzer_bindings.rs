//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact analyzer binding restoration and malformed persistent envelope rejection.

use uqa_analysis::{keyword_analyzer, whitespace_analyzer, AnalyzerLimits, AnalyzerResources};
use uqa_storage::{AnalyzerBindingOwner, AnalyzerPhase, FieldAnalyzerBinding};

#[test]
fn japanese_analyzers_require_lossless_revision_storage_when_their_feature_is_available() {
    use uqa_storage::inverted_index::{validate_linear_analyzer, validate_linear_revision};
    let config = serde_json::from_value::<uqa_analysis::Analyzer>(serde_json::json!({
        "tokenizer": {"type": "kuromoji_tokenizer"},
    }));
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `kuromoji_tokenizer`"));
            return;
        }
    };
    assert!(config.uses_japanese_stages());
    assert!(validate_linear_analyzer(&config).is_err());
    assert!(validate_linear_revision(&config.compile().unwrap()).is_err());
    for component in [
        "kuromoji_baseform",
        "kuromoji_stemmer",
        "kuromoji_hiragana_uppercase",
        "kuromoji_katakana_uppercase",
        "kuromoji_readingform",
        "kuromoji_number",
    ] {
        let config: uqa_analysis::Analyzer = serde_json::from_value(serde_json::json!({
            "token_filters": [{"type": component}],
        }))
        .unwrap();
        assert!(validate_linear_analyzer(&config).is_err(), "{component}");
        assert!(
            validate_linear_revision(&config.compile().unwrap()).is_err(),
            "{component}"
        );
    }
    assert!(validate_linear_analyzer(&keyword_analyzer()).is_ok());
}

fn binding() -> FieldAnalyzerBinding {
    let index = whitespace_analyzer().compile().unwrap();
    FieldAnalyzerBinding::unassigned(index.clone(), index.clone())
        .assigned(
            "index_revision",
            index,
            AnalyzerPhase::Index,
            AnalyzerBindingOwner::Field,
        )
        .assigned(
            "search_revision",
            keyword_analyzer().compile().unwrap(),
            AnalyzerPhase::Search,
            AnalyzerBindingOwner::Field,
        )
}

#[test]
fn binding_restores_independent_revisions_without_a_name_registry() {
    let binding = binding();
    let json = binding.to_json().unwrap();
    let restored =
        FieldAnalyzerBinding::from_json(&json, &AnalyzerResources::new(AnalyzerLimits::default()))
            .unwrap();
    assert_eq!(
        restored.index.compiled.descriptor().fingerprint(),
        binding.index.compiled.descriptor().fingerprint()
    );
    assert_eq!(
        restored.search.compiled.descriptor().fingerprint(),
        binding.search.compiled.descriptor().fingerprint()
    );
    assert_eq!(restored.to_json().unwrap(), json);
    assert!(restored.uses_name("index_revision"));
    assert!(restored.uses_name("search_revision"));
    assert_eq!(
        restored.last_assignment(),
        Some(("search_revision".into(), "search".into()))
    );
}

#[test]
fn binding_rejects_corruption_unknown_properties_and_competing_gin_sides() {
    let original =
        serde_json::from_str::<serde_json::Value>(&binding().to_json().unwrap()).unwrap();
    for (path, replacement) in [
        ("/version", serde_json::json!(2)),
        ("/format", serde_json::json!("other")),
        ("/owner", serde_json::json!("gin")),
        ("/last_phase", serde_json::json!("both")),
        ("/search/name", serde_json::Value::Null),
        ("/index/name", serde_json::json!(" leading_space")),
        ("/search/name", serde_json::json!("")),
        (
            "/index/descriptor/fingerprint",
            serde_json::json!("0".repeat(64)),
        ),
        (
            "/search/descriptor/fingerprint",
            serde_json::json!("0".repeat(64)),
        ),
    ] {
        let mut malformed = original.clone();
        *malformed.pointer_mut(path).unwrap() = replacement;
        assert!(
            FieldAnalyzerBinding::from_json(&malformed.to_string(), &AnalyzerResources::default())
                .is_err(),
            "accepted {path}"
        );
    }
    let mut unknown = original.clone();
    unknown["index"]["ignored"] = serde_json::json!(true);
    assert!(
        FieldAnalyzerBinding::from_json(&unknown.to_string(), &AnalyzerResources::default())
            .is_err()
    );
    // Equal duplicate values must still reach the descriptor's strict parser unchanged.
    let duplicate = original.to_string().replacen(
        "\"fingerprint\":",
        "\"fingerprint\":\"duplicate\",\"fingerprint\":",
        1,
    );
    assert!(FieldAnalyzerBinding::from_json(&duplicate, &AnalyzerResources::default()).is_err());
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_descriptor_bytes: 10,
        ..AnalyzerLimits::default()
    });
    assert!(
        FieldAnalyzerBinding::from_json(&original.to_string(), &resources)
            .unwrap_err()
            .to_string()
            .contains("limits")
    );
}
