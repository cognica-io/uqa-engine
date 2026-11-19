//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Image dependencies survive cache eviction and source loss without re-resolving names.

use super::*;
use crate::{
    Analyzer, AnalyzerDescriptor, AnalyzerLimits, AnalyzerResourceInput, AnalyzerResourceKind,
    AnalyzerResources, NormalizationConfig, Tokenizer,
};
use uqa_core::memory::{Budgeted, MemoryBudget};

fn source_config() -> Analyzer {
    Analyzer::new(
        Tokenizer::Nori(crate::nori::NoriTokenizerConfig {
            decompound_mode: crate::nori::DecompoundMode::None,
            ..Default::default()
        }),
        Vec::new(),
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::Unavailable)
}

fn image_input(encoded: &[u8], memory: &MemoryBudget) -> AnalyzerResourceInput {
    let lease = memory.reserve(encoded.len()).unwrap();
    AnalyzerResourceInput {
        kind: AnalyzerResourceKind::Nori,
        sha256: ResourceHash::of(encoded).into_bytes(),
        bytes: Budgeted::new(encoded.to_vec(), lease)
            .into_shared()
            .unwrap(),
    }
}

#[test]
fn analyzer_image_never_uses_the_original_cache_or_resolver_for_missing_artifacts() {
    let encoded = bytes(-10);
    let calls = Arc::new(AtomicUsize::new(0));
    let resources = NoriResources::with_resolver(
        Arc::new({
            let encoded = encoded.clone();
            let calls = calls.clone();
            move |_: &DictionaryRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(super::artifact(encoded.clone())))
            }
        }),
        ResourceLimits::default(),
    );
    let owner = AnalyzerResources::with_nori_resources(AnalyzerLimits::default(), resources);
    let source = owner.compile(&source_config()).unwrap();
    let before = calls.load(Ordering::SeqCst);
    assert!(owner
        .restore_image(source.descriptor().clone(), Vec::new())
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let memory = MemoryBudget::new(1 << 20);
    let restored = owner
        .restore_image(
            source.descriptor().clone(),
            vec![image_input(&encoded, &memory)],
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&restored, &source));
    assert_eq!(calls.load(Ordering::SeqCst), before);
    assert!(memory.used() > 0);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn analyzer_image_validates_hash_complete_resource_closure_and_destination_limits() {
    let encoded = bytes(-10);
    let owner = AnalyzerResources::with_nori_resources(
        AnalyzerLimits::default(),
        supplied(encoded.clone(), ResourceLimits::default()),
    );
    let source = owner.compile(&source_config()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut corrupt = encoded.to_vec();
    corrupt[0] ^= 0xff;
    let mut input = image_input(&corrupt, &memory);
    input.sha256 = ResourceHash::of(&encoded).into_bytes();
    assert!(matches!(
        owner.restore_image(source.descriptor().clone(), vec![input]),
        Err(crate::AnalysisError::Dictionary(
            DictionaryError::ResourceHashMismatch { .. }
        ))
    ));
    assert_eq!(memory.used(), 0);
    let mut extra = vec![
        image_input(&encoded, &memory),
        image_input(&bytes(123), &memory),
    ];
    extra.sort_by_key(|input| (input.kind, input.sha256));
    assert!(matches!(
        owner.restore_image(source.descriptor().clone(), extra),
        Err(crate::AnalysisError::Descriptor(
            "image resources differ from the descriptor closure"
        ))
    ));
    assert_eq!(memory.used(), 0);
    let target = AnalyzerResources::with_nori_resources(
        AnalyzerLimits::default(),
        NoriResources::with_resolver(
            Arc::new(|_: &DictionaryRequest| panic!("image consulted a host resolver")),
            ResourceLimits {
                dictionary: DictionaryLimits {
                    max_encoded_bytes: encoded.len() - 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    );
    assert!(matches!(
        target.restore_image(
            source.descriptor().clone(),
            vec![image_input(&encoded, &memory)]
        ),
        Err(crate::AnalysisError::ResourceLimit {
            resource: "image dictionary bytes",
            ..
        })
    ));
    assert_eq!(memory.used(), 0);
}
#[test]
fn compiled_korean_image_retains_exact_artifact_after_source_and_cache_loss() {
    let encoded = bytes(-10);
    let weak = Arc::downgrade(&encoded);
    let expected_hash = ResourceHash::of(&encoded);
    let resources = supplied(
        encoded,
        ResourceLimits {
            max_cached_dictionaries: 0,
            ..Default::default()
        },
    );
    let owner = AnalyzerResources::with_nori_resources(
        AnalyzerLimits {
            max_cached_analyzers: 0,
            ..Default::default()
        },
        resources,
    );
    let config = Analyzer::new(
        Tokenizer::Nori(crate::nori::NoriTokenizerConfig {
            decompound_mode: crate::nori::DecompoundMode::None,
            ..Default::default()
        }),
        Vec::new(),
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::Unavailable);
    let compiled = owner.compile(&config).unwrap();
    let descriptor = compiled.descriptor().canonical_json().to_owned();
    let expected = compiled.analyze_tokens("가나").unwrap();
    drop(owner);
    assert!(weak.upgrade().is_some());
    let memory = MemoryBudget::new(1 << 20);
    let copied: Arc<Budgeted<Vec<u8>>> = {
        let mut artifacts = compiled.dictionary_resources();
        let artifact = artifacts.next().unwrap();
        assert!(artifacts.next().is_none());
        assert_eq!(artifact.kind(), AnalyzerResourceKind::Nori);
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
                kind: AnalyzerResourceKind::Nori,
                sha256: expected_hash.into_bytes(),
                bytes: copied,
            }],
        )
        .unwrap();
    assert_eq!(restored.analyze_tokens("가나").unwrap(), expected);
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
fn compiled_korean_image_deduplicates_shared_tokenizer_and_profile_artifacts() {
    let resources = supplied(bytes(-10), ResourceLimits::default());
    let owner = AnalyzerResources::with_nori_resources(AnalyzerLimits::default(), resources);
    let config = Analyzer::new(
        Tokenizer::Nori(crate::nori::NoriTokenizerConfig {
            decompound_mode: crate::nori::DecompoundMode::None,
            ..Default::default()
        }),
        Vec::new(),
        Vec::new(),
    )
    .with_normalization(NormalizationConfig::UnicodeSimpleLowercase {
        profile: crate::UnicodeProfile::Nori {
            dictionary: crate::nori::DEFAULT_NORI_DICTIONARY.into(),
        },
    });
    let compiled = owner.compile(&config).unwrap();
    assert_eq!(compiled.dictionary_resources().count(), 1);
    assert_eq!(
        compiled.dictionary_resources().next().unwrap().kind(),
        AnalyzerResourceKind::Nori
    );
}
