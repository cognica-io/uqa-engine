//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_core::FieldName;
use uqa_engine::Engine;
use uqa_scoring::{
    VectorCalibrationModel, VectorCalibrationProvenance, VectorCalibrationTarget,
    VectorProbabilityTransform,
};
use uqa_storage::document_store::Document;

fn target(k: usize) -> VectorCalibrationTarget {
    VectorCalibrationTarget {
        corpus_id: "public.docs".into(),
        corpus_version: "sha256:docs-v1".into(),
        index_id: "public.docs.embedding".into(),
        index_version: "sha256:embedding-index-v1".into(),
        index_kind: "memory-bruteforce".into(),
        embedding_model_id: "fixture-encoder".into(),
        embedding_model_version: "1.0.0".into(),
        candidate_k: k,
        dimensions: 2,
    }
}

fn model(k: usize) -> VectorCalibrationModel {
    VectorCalibrationModel::new(
        VectorProbabilityTransform::new(0.05, 0.9, 0.3, 0.2).unwrap(),
        VectorCalibrationProvenance {
            model_version: "fixture-calibrator-v1".into(),
            target: target(k),
            fit_sample_count: 500,
        },
    )
    .unwrap()
}

fn add_vector(engine: &Engine, doc_id: u64, vector: Vec<f32>) {
    engine
        .add_document_with_vectors(
            "docs",
            doc_id,
            Document::new(),
            BTreeMap::<FieldName, Vec<f32>>::from([("embedding".into(), vector)]),
        )
        .unwrap();
}

#[test]
fn model_based_vector_search_validates_provenance_and_does_not_refit_per_query() {
    let engine = Engine::new();
    engine.create_default_table("docs", Vec::new()).unwrap();
    engine.create_vector_field("docs", "embedding", 2).unwrap();
    add_vector(&engine, 1, vec![1.0, 0.0]);
    add_vector(&engine, 2, vec![0.8, 0.2]);
    add_vector(&engine, 3, vec![0.0, 1.0]);

    let model = model(3);
    engine
        .save_vector_calibration_model("docs-embedding", &model)
        .unwrap();
    assert_eq!(
        engine
            .load_vector_calibration_model("docs-embedding")
            .unwrap(),
        Some(model.clone())
    );

    let result = engine
        .calibrated_vector_search_with_model("docs", "embedding", [1.0, 0.0], &model, &target(3))
        .unwrap();
    assert_eq!(
        result.iter().map(|entry| entry.doc_id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(result.windows(2).all(|pair| pair[0].score >= pair[1].score));
    assert!(result
        .iter()
        .all(|entry| (0.0..=1.0).contains(&entry.score)));

    let error = engine
        .calibrated_vector_search_with_model("docs", "embedding", [1.0, 0.0], &model, &target(2))
        .unwrap_err();
    assert!(error.to_string().contains("target mismatch"), "{error}");
}

#[test]
fn vector_calibration_model_round_trips_through_the_persistent_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vector-calibration.db");
    let expected = model(3);
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .save_vector_calibration_model("release-v1", &expected)
            .unwrap();
    }
    {
        let engine = Engine::open(&path).unwrap();
        assert_eq!(
            engine.load_vector_calibration_model("release-v1").unwrap(),
            Some(expected)
        );
        assert!(engine.drop_vector_calibration_model("release-v1").unwrap());
        assert!(engine
            .load_vector_calibration_model("release-v1")
            .unwrap()
            .is_none());
    }
}
