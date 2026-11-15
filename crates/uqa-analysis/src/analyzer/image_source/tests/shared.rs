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
