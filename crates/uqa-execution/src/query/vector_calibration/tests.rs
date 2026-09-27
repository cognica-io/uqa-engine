//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::{
    build::DiskANNTemporaryBudget, DiskANNIndexOptions, DiskANNMemoryIndex,
};
use uqa_storage::vector_index::DiskANNIndexParams;

#[test]
fn diskann_calibration_runtime_validation_uses_actual_selected_versions() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = DiskANNMemoryIndex::new(
        2,
        DiskANNIndexOptions::for_parameters(DiskANNIndexParams::for_dimensions(2).unwrap()),
        &DiskANNTemporaryBudget::new(1 << 20),
        &control,
    )
    .unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    let target = diskann_target(
        &index,
        "public.docs",
        "embedding",
        ("fixture", "1"),
        3,
        &control,
    )
    .unwrap();
    validate_index(&index, &target, &control).unwrap();
    let model = VectorCalibrationModel::new(
        uqa_scoring::VectorProbabilityTransform::new(0.0, 1.0, 1.0, 0.5).unwrap(),
        uqa_scoring::VectorCalibrationProvenance {
            model_version: "fixed".into(),
            target: target.clone(),
            fit_sample_count: 100,
        },
    )
    .unwrap();
    validate_names(&model, &target, "public.docs", "embedding").unwrap();
    assert!(validate_names(&model, &target, "public.other", "embedding").is_err());
    assert!(validate_names(&model, &target, "public.docs", "other").is_err());
    let mut wrong = target.clone();
    wrong.index_kind = "hnsw".into();
    assert!(validate_index(&index, &wrong, &control).is_err());
    wrong = target.clone();
    wrong.dimensions = 3;
    assert!(validate_index(&index, &wrong, &control).is_err());
    index.add(2, vec![0.0, 1.0]).unwrap();
    assert!(validate_index(&index, &target, &control)
        .unwrap_err()
        .to_string()
        .contains("target mismatch"));
    let fresh = diskann_target(
        &index,
        "public.docs",
        "embedding",
        ("fixture", "1"),
        3,
        &control,
    )
    .unwrap();
    assert_ne!(target.corpus_version, fresh.corpus_version);
    assert_eq!(target.index_version, fresh.index_version);
    assert!(model.validate_for(&fresh).is_err());
}
