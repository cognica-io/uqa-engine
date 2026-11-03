//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::{Budgeted, MemoryBudget};

fn empty_bytes() -> Arc<Budgeted<Vec<u8>>> {
    let memory = MemoryBudget::new(1024);
    Budgeted::new(Vec::new(), memory.empty_reservation())
        .into_shared()
        .unwrap()
}

#[test]
fn analyzer_image_restores_inline_inputs_without_resolving_a_resource() {
    let owner = AnalyzerResources::default();
    let source = owner.compile(&crate::standard_analyzer("english")).unwrap();
    let restored = owner
        .restore_image(source.descriptor().clone(), Vec::new())
        .unwrap();
    assert_eq!(
        source.analyze_tokens("The cats").unwrap(),
        restored.analyze_tokens("The cats").unwrap()
    );
    assert_eq!(
        source.descriptor().canonical_json(),
        restored.descriptor().canonical_json()
    );
    assert_eq!(restored.dictionary_resources().count(), 0);
}

#[test]
fn analyzer_image_preserves_resolved_synonyms_after_file_and_owner_loss() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "quick => fast, rapid\n").unwrap();
    let config = crate::Analyzer::new(
        crate::Tokenizer::Whitespace,
        vec![crate::TokenFilter::synonym_from_path(&path).unwrap()],
        Vec::new(),
    );
    let owner = AnalyzerResources::new(crate::AnalyzerLimits::default());
    let source = owner.compile(&config).unwrap();
    let expected = source.analyze_tokens("quick fox").unwrap();
    let descriptor = source.descriptor().clone();
    drop(source);
    drop(owner);
    directory.close().unwrap();
    assert!(config.compile().is_err());
    let restored = AnalyzerResources::new(crate::AnalyzerLimits::default())
        .restore_image(descriptor, Vec::new())
        .unwrap();
    assert_eq!(restored.analyze_tokens("quick fox").unwrap(), expected);
    assert_eq!(restored.dictionary_resources().count(), 0);
}

#[test]
fn analyzer_image_rejects_duplicate_resources_before_reconstruction() {
    let owner = AnalyzerResources::default();
    let source = owner.compile(&crate::Analyzer::default()).unwrap();
    let inputs = (0..2)
        .map(|_| AnalyzerResourceInput {
            kind: AnalyzerResourceKind::Nori,
            sha256: [1; 32],
            bytes: empty_bytes(),
        })
        .collect();
    assert!(matches!(
        owner.restore_image(source.descriptor().clone(), inputs),
        Err(AnalysisError::Descriptor(
            "unordered or duplicate image resources"
        ))
    ));
}

#[cfg(any(not(feature = "nori"), not(feature = "kuromoji")))]
#[test]
fn analyzer_image_rejects_unavailable_resource_features_before_payload_use() {
    let owner = AnalyzerResources::default();
    let source = owner.compile(&crate::Analyzer::default()).unwrap();
    for kind in [AnalyzerResourceKind::Nori, AnalyzerResourceKind::Kuromoji] {
        let enabled = match kind {
            AnalyzerResourceKind::Nori => cfg!(feature = "nori"),
            AnalyzerResourceKind::Kuromoji => cfg!(feature = "kuromoji"),
        };
        if !enabled {
            assert!(owner.image_resource_byte_limit(kind).is_err());
            assert!(owner
                .restore_image(
                    source.descriptor().clone(),
                    vec![AnalyzerResourceInput {
                        kind,
                        sha256: [0; 32],
                        bytes: empty_bytes()
                    }]
                )
                .is_err());
        }
    }
}
