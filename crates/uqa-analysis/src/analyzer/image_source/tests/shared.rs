//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{decode_image, ImageIoError};

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Input(#[from] ImageIoError),
    #[error(transparent)]
    Retention(#[from] ValueRetentionError),
}

#[test]
fn restored_analyzer_image_keeps_request_ownership_out_of_shared_pipeline_caches() {
    for populated in [false, true] {
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        let compiled = resources.compile(&Analyzer::default()).unwrap();
        let original = populated.then(|| {
            compiled
                .image_source_with_control(&ProductionControl::uncontrolled())
                .unwrap()
        });
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let restored = decode_image(
            &mut std::io::empty(),
            &memory,
            &cancellation,
            async |mut input| Ok::<_, Failure>(compiled.image_source_with_input(&mut input).await?),
        )
        .unwrap();
        assert!(memory.used() > 0);
        let reader = compiled
            .image_source_with_control(&ProductionControl::uncontrolled())
            .unwrap();
        assert!(!Arc::ptr_eq(&restored.0, &reader.0));
        assert_eq!(restored.canonical_json(), reader.canonical_json());
        if let Some(original) = &original {
            assert!(Arc::ptr_eq(&original.0, &reader.0));
        }
        drop(restored);
        assert_eq!(memory.used(), 0);
        assert_eq!(
            reader.canonical_json(),
            compiled.descriptor().canonical_json()
        );
        assert_eq!(
            compiled.analyze("unrelated reader").unwrap(),
            ["unrelated", "reader"]
        );
    }
}

#[test]
fn catalog_image_owners_release_each_publication_allowance_independently() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = [
        resources.compile(&crate::whitespace_analyzer()).unwrap(),
        resources.compile(&crate::keyword_analyzer()).unwrap(),
    ];
    let first = MemoryBudget::new(1 << 20);
    let second = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let images = |memory| {
        let control = ProductionControl::new(memory, &cancellation, &cancellation);
        compiled
            .each_ref()
            .map(|analyzer| analyzer.image_source_with_control(&control).unwrap())
    };
    let a = images(&first);
    let required = first.used();
    assert!(required > 0);
    let b = images(&second);
    drop(a);
    let after_first = (first.used(), second.used());
    drop(b);
    assert_eq!((first.used(), second.used()), (0, 0));
    assert_eq!(after_first, (0, required));
}

#[test]
fn interleaved_publishers_reuse_only_their_own_live_image() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = resources.compile(&crate::whitespace_analyzer()).unwrap();
    let token = CancellationToken::new();
    let parent = MemoryBudget::new(1 << 20);
    let first = parent.child(1 << 16);
    let control = ProductionControl::new(&first, &token, &token);
    let a = compiled.image_source_with_control(&control).unwrap();
    let required = first.used();
    let ordinary = compiled
        .image_source_with_control(&ProductionControl::uncontrolled())
        .unwrap();
    let second = parent.child(required);
    let other = compiled
        .image_source_with_control(&ProductionControl::new(&second, &token, &token))
        .unwrap();
    assert!(!Arc::ptr_eq(&a.0, &ordinary.0));
    assert!(!Arc::ptr_eq(&a.0, &other.0));
    assert_eq!(second.used(), required);
    assert_eq!(parent.used(), 2 * required);

    let short = parent.child(required - 1);
    assert!(matches!(
        compiled.image_source_with_control(&ProductionControl::new(&short, &token, &token)),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(short.used(), 0);
    let full = first.reserve(first.available()).unwrap();
    let alias = first.clone();
    let same = compiled
        .image_source_with_control(&ProductionControl::new(&alias, &token, &token))
        .unwrap();
    assert!(Arc::ptr_eq(&a.0, &same.0));
    let ordinary_again = compiled
        .image_source_with_control(&ProductionControl::uncontrolled())
        .unwrap();
    assert!(Arc::ptr_eq(&ordinary.0, &ordinary_again.0));

    drop((full, a, same));
    assert_eq!(first.used(), 0);
    assert_eq!(parent.used(), required);
    drop(other);
    assert_eq!(parent.used(), 0);
    assert_eq!(
        ordinary.canonical_json(),
        compiled.descriptor().canonical_json()
    );
}
