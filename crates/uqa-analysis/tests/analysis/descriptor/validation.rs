//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_analysis::{AnalysisError, AnalyzerFingerprint};

use super::*;

fn rehash(wire: &mut Value) -> String {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"UQA analyzer descriptor\0");
    hash.update(serde_json::to_vec(&canonical(&wire["descriptor"])).unwrap());
    wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
    serde_json::to_string(wire).unwrap()
}

#[test]
fn restoration_rejects_corruption_unknown_revisions_and_mutable_or_implicit_inputs() {
    let descriptor = resolve(&keyword());
    let source: Value = serde_json::from_str(descriptor.canonical_json()).unwrap();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut wire = source.clone();
    wire["descriptor"]["length_policy"] = json!("discount_overlaps");
    assert!(matches!(
        resources.restore_json(&wire.to_string()),
        Err(AnalysisError::DescriptorFingerprint { .. })
    ));
    for key in [
        "format_version",
        "algorithm_revision",
        "source_mapping_revision",
    ] {
        wire = source.clone();
        wire["descriptor"][key] = json!(2);
        assert!(matches!(
            resources.restore_json(&rehash(&mut wire)),
            Err(AnalysisError::DescriptorRevision { actual: 2, .. })
        ));
    }
    for (pointer, value) in [
        ("/descriptor/format", json!("other")),
        (
            "/descriptor/pipeline/tokenizer",
            json!({"type":"keyword","unknown":true}),
        ),
        (
            "/descriptor/pipeline/token_filters",
            json!([{"type":"stop"}]),
        ),
        (
            "/descriptor/pipeline/token_filters",
            json!([{"type":"synonym","synonyms":{},"synonyms_path":"/descriptor-must-not-read-this-file"}]),
        ),
        (
            "/descriptor/runtime_profiles/rust_unicode",
            json!([0, 0, 0]),
        ),
        (
            "/descriptor/runtime_profiles/expressions",
            json!(["corrupt-profile"]),
        ),
    ] {
        wire = source.clone();
        *wire.pointer_mut(pointer).unwrap() = value;
        assert!(
            matches!(
                resources.restore_json(&rehash(&mut wire)),
                Err(AnalysisError::Descriptor(_))
            ),
            "{wire}"
        );
    }
    wire = source.clone();
    wire["descriptor"]["unknown"] = json!(true);
    assert!(matches!(
        resources.restore_json(&rehash(&mut wire)),
        Err(AnalysisError::Json(_))
    ));
    wire = source;
    wire["descriptor"]["pipeline"]
        .as_object_mut()
        .unwrap()
        .remove("tokenizer");
    assert!(matches!(
        resources.restore_json(&rehash(&mut wire)),
        Err(AnalysisError::Descriptor(_))
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
    assert_eq!(
        resources
            .compile(&keyword())
            .unwrap()
            .analyze("ok")
            .unwrap(),
        ["ok"]
    );
}

#[test]
fn descriptor_fingerprints_parse_only_complete_hexadecimal_digests() {
    let text = resolve(&keyword()).fingerprint().to_string();
    assert_eq!(
        text.parse::<AnalyzerFingerprint>().unwrap().to_string(),
        text
    );
    assert_eq!(
        text.to_uppercase()
            .parse::<AnalyzerFingerprint>()
            .unwrap()
            .to_string(),
        text
    );
    for invalid in ["", "0", &text[..63], &"g".repeat(64), &"é".repeat(32)] {
        assert!(invalid.parse::<AnalyzerFingerprint>().is_err());
    }
}

#[test]
fn restoration_rejects_duplicate_keys_and_omitted_runtime_profile_fields() {
    let descriptor = resolve(&keyword());
    let json = descriptor.canonical_json();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    for (needle, replacement) in [
        (
            "\"type\":\"keyword\"",
            "\"type\":\"standard\",\"type\":\"keyword\"",
        ),
        (
            "\"rust_unicode\":null",
            "\"rust_unicode\":null,\"rust_unicode\":null",
        ),
        (
            "\"format_version\":1",
            "\"format_version\":1,\"format_version\":1",
        ),
    ] {
        assert!(matches!(
            resources.restore_json(&json.replace(needle, replacement)),
            Err(AnalysisError::Json(_))
        ));
    }
    for key in ["rust_unicode", "normalization_unicode"] {
        let mut wire: Value = serde_json::from_str(json).unwrap();
        wire["descriptor"]["runtime_profiles"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(matches!(
            resources.restore_json(&rehash(&mut wire)),
            Err(AnalysisError::Json(_))
        ));
    }
    assert_eq!(resources.cache_stats().analyzers, 0);
}

#[test]
fn descriptor_limits_apply_before_parse_file_read_and_cached_publication() {
    let descriptor = resolve(&keyword());
    let length = descriptor.canonical_json().len();
    let mut limits = AnalyzerLimits {
        max_descriptor_bytes: length - 1,
        ..AnalyzerLimits::default()
    };
    let resources = AnalyzerResources::new(limits);
    assert!(matches!(
        resources.compile(&keyword()),
        Err(AnalysisError::ResourceLimit { .. })
    ));
    assert!(matches!(
        resources.restore(descriptor.clone()),
        Err(AnalysisError::ResourceLimit { .. })
    ));
    assert!(matches!(
        resources.restore_json(&" ".repeat(length)),
        Err(AnalysisError::ResourceLimit { .. })
    ));
    limits.max_descriptor_bytes = length;
    assert!(AnalyzerResources::new(limits)
        .restore_json(descriptor.canonical_json())
        .is_ok());
    limits.max_stages = 0;
    let resources = AnalyzerResources::new(limits);
    assert!(matches!(
        resources.restore(descriptor),
        Err(AnalysisError::ResourceLimit {
            resource: "analyzer stages",
            ..
        })
    ));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "#".repeat(4097)).unwrap();
    let config = Analyzer::new(
        Tokenizer::Keyword,
        vec![TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(path.clone()),
        }],
        Vec::new(),
    );
    let resources = AnalyzerResources::new(AnalyzerLimits {
        max_descriptor_bytes: 4096,
        ..AnalyzerLimits::default()
    });
    assert!(matches!(
        resources.compile(&config),
        Err(AnalysisError::ResourceLimit {
            resource: "synonym source bytes",
            ..
        })
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
    std::fs::write(&path, [0xff]).unwrap();
    assert!(matches!(
        resources.compile(&config),
        Err(AnalysisError::SynonymFile(_))
    ));
    let group = (0..100)
        .map(|index| format!("term{index}"))
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(&path, group).unwrap();
    assert!(matches!(
        resources.compile(&config),
        Err(AnalysisError::ResourceLimit {
            resource: "resolved synonym bytes",
            ..
        })
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
    std::fs::write(&path, "# empty rules\n").unwrap();
    let compiled = resources.compile(&config).unwrap();
    assert_eq!(compiled.analyze("ok").unwrap(), ["ok"]);
    assert_eq!(resources.cache_stats().analyzers, 1);
}

#[test]
fn failed_regex_preparation_does_not_publish_a_resolved_revision() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut config = keyword();
    config.tokenizer = Tokenizer::Pattern {
        pattern: "\\w{1000000}".into(),
    };
    let descriptor = resolve(&config);
    assert!(matches!(
        resources.restore(descriptor),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
    config.tokenizer = Tokenizer::Pattern {
        pattern: "[".into(),
    };
    assert!(matches!(
        resources.compile(&config),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    assert_eq!(resources.cache_stats().analyzers, 0);
    assert_eq!(
        resources
            .compile(&keyword())
            .unwrap()
            .analyze("ok")
            .unwrap(),
        ["ok"]
    );
}
