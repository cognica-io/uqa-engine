//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese image dependencies distinguish retained profiles from already expanded sets.

use super::*;
use crate::{
    Analyzer, AnalyzerDescriptor, AnalyzerLimits, AnalyzerResourceInput, AnalyzerResourceKind,
    AnalyzerResources, NormalizationConfig, TokenFilter, Tokenizer,
};
use uqa_core::memory::{Budgeted, MemoryBudget};

fn owner(encoded: Arc<[u8]>) -> AnalyzerResources {
    AnalyzerResources::builder(AnalyzerLimits {
        max_cached_analyzers: 0,
        ..Default::default()
    })
    .kuromoji_resources(KuromojiResources::with_resolver(
        Arc::new(move |_: &DictionaryRequest| {
            Ok(Some(DictionaryArtifact {
                sha256: ResourceHash::of(&encoded),
                bytes: DictionaryBytes::Shared(encoded.clone()),
            }))
        }),
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..Default::default()
        },
    ))
    .build()
}

#[test]
fn compiled_japanese_image_retains_exact_artifact_after_source_and_cache_loss() {
    let encoded: Arc<[u8]> = crate::kuromoji::tests::fixtures::bundle().into();
    let weak = Arc::downgrade(&encoded);
    let expected_hash = ResourceHash::of(&encoded);
    let resources = owner(encoded);
    let config = Analyzer::new(
        Tokenizer::Kuromoji(crate::kuromoji::KuromojiTokenizerConfig::default()),
        Vec::new(),
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::Unavailable);
    let compiled = resources.compile(&config).unwrap();
    let descriptor = compiled.descriptor().canonical_json().to_owned();
    let expected = compiled.analyze_tokens("令和 食べた").unwrap();
    drop(resources);
    assert!(weak.upgrade().is_some());
    let memory = MemoryBudget::new(1 << 20);
    let copied: Arc<Budgeted<Vec<u8>>> = {
        let mut artifacts = compiled.dictionary_resources();
        let artifact = artifacts.next().unwrap();
        assert!(artifacts.next().is_none());
        assert_eq!(artifact.kind(), AnalyzerResourceKind::Kuromoji);
        assert_eq!(artifact.sha256(), expected_hash.into_bytes());
        let lease = memory.reserve(artifact.bytes().len()).unwrap();
        Budgeted::new(artifact.bytes().to_vec(), lease)
            .into_shared()
            .unwrap()
    };
    drop(compiled);
    assert!(weak.upgrade().is_none());
    let restored_owner = AnalyzerResources::default();
    let restored = restored_owner
        .restore_image(
            AnalyzerDescriptor::from_json(&descriptor, restored_owner.limits()).unwrap(),
            vec![AnalyzerResourceInput {
                kind: AnalyzerResourceKind::Kuromoji,
                sha256: expected_hash.into_bytes(),
                bytes: copied,
            }],
        )
        .unwrap();
    assert_eq!(restored.analyze_tokens("令和 食べた").unwrap(), expected);
    assert_eq!(restored.descriptor().canonical_json(), descriptor);
    assert_eq!(restored.dictionary_resources().count(), 1);
    assert!(memory.used() > 0);
    let retained = restored.clone();
    drop(restored);
    assert!(memory.used() > 0);
    drop(retained);
    assert_eq!(memory.used(), 0);
}

#[test]
fn japanese_expanded_stop_sets_require_no_original_dictionary_in_the_image() {
    let encoded: Arc<[u8]> = crate::kuromoji::tests::fixtures::bundle().into();
    let resources = owner(encoded);
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::KuromojiPartOfSpeech(crate::kuromoji::KuromojiPOSConfig::default()),
            TokenFilter::KuromojiStop(crate::kuromoji::KuromojiStopConfig {
                ignore_case: false,
                ..Default::default()
            }),
        ],
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::Unavailable);
    let compiled = resources.compile(&config).unwrap();
    assert_eq!(compiled.dictionary_resources().count(), 0);
    let restored_owner = AnalyzerResources::builder(AnalyzerLimits::default())
        .kuromoji_resources(KuromojiResources::with_resolver(
            Arc::new(|_: &DictionaryRequest| panic!("resolved stop sets consulted a resolver")),
            ResourceLimits::default(),
        ))
        .build();
    let restored = restored_owner
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    assert_eq!(restored.analyze("は token").unwrap(), ["token"]);
    assert_eq!(
        restored.analyze_tokens("は token").unwrap(),
        compiled.analyze_tokens("は token").unwrap()
    );
    assert_eq!(restored.dictionary_resources().count(), 0);
}
