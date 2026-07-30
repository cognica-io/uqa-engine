//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed deep-model persistence: save a model on one engine
//! instance, drop the engine, reopen the same `SQLite` path, and verify
//! the model rehydrates and produces identical predictions.

use tempfile::tempdir;
use uqa_engine::Engine;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

fn linear_model() -> DeepModel {
    DeepModel {
        layers: vec![
            DeepLayerSpec::Embed {
                embedding: vec![3.0, 1.0],
            },
            DeepLayerSpec::Flatten,
            DeepLayerSpec::Dense {
                weights: vec![1.0, 0.0, 0.0, 1.0],
                bias: vec![0.0, 0.0],
                output_channels: 2,
                input_channels: 2,
            },
            DeepLayerSpec::Softmax,
        ],
        alpha: 0.0,
        gating: GatingSpec::None,
    }
}

#[test]
fn deep_model_round_trips_through_sqlite_catalog() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("uqa.db");

    let initial_predictions = {
        let engine = Engine::open(&path).expect("open initial engine");
        engine
            .save_model("clf", &linear_model())
            .expect("save model");
        engine
            .deep_predict("clf")
            .expect("read initial prediction")
            .expect("predict from initial")
    };
    assert!(!initial_predictions.is_empty());

    // Drop and reopen the same database; the model must rehydrate from
    // the catalog without `save_model` being called again.
    {
        let engine = Engine::open(&path).expect("reopen engine");
        let model = engine
            .load_model("clf")
            .expect("read model catalog")
            .expect("model rehydrated from catalog");
        assert_eq!(model.layers.len(), 4);
        let reopened_predictions = engine
            .deep_predict("clf")
            .expect("read reopened prediction")
            .expect("predict after reopen");
        assert_eq!(initial_predictions.len(), reopened_predictions.len());
        for ((a_id, a_score), (b_id, b_score)) in
            initial_predictions.iter().zip(reopened_predictions.iter())
        {
            assert_eq!(a_id, b_id);
            assert!(
                (a_score - b_score).abs() < 1e-9,
                "score drift across reopen: {a_score} vs {b_score}"
            );
        }
    }

    // drop_model should remove from both cache and the catalog.
    {
        let engine = Engine::open(&path).expect("reopen for drop");
        engine.drop_model("clf").unwrap();
    }
    {
        let engine = Engine::open(&path).expect("reopen after drop");
        assert!(
            engine.load_model("clf").unwrap().is_none(),
            "model survived drop"
        );
    }
}
