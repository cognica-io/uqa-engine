//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `deep_predict` SQL function and engine
//! deep-model persistence.

use uqa_core::Value;
use uqa_engine::deep::{DeepLayerSpec, DeepModel, GatingSpec};
use uqa_engine::Engine;

fn linear_classifier_model() -> DeepModel {
    // Inputs: 2-element embedding. Linear projection picks the
    // larger feature into class 0.
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
fn save_load_drop_round_trips_through_engine() {
    let engine = Engine::new();
    let model = linear_classifier_model();
    engine.save_model("clf", &model).unwrap();
    let loaded = engine.load_model("clf").unwrap();
    assert_eq!(loaded.layers.len(), model.layers.len());

    let scores = engine.deep_predict("clf").unwrap();
    assert_eq!(scores.len(), 1);
    // The softmax row is doc_id=1 (the smallest after Flatten).
    assert!(scores[0].1 > 0.5);

    engine.drop_model("clf");
    assert!(engine.load_model("clf").is_none());
}

#[test]
fn deep_predict_sql_function_returns_per_doc_scores() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE seeds (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO seeds (id) VALUES (1)", &[])
        .unwrap();
    let model = linear_classifier_model();
    engine.save_model("clf", &model).unwrap();

    let result = engine
        .sql(
            "SELECT id, _score FROM seeds WHERE deep_predict('clf')",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let id = match result.rows[0].get("id") {
        Some(Value::Int(n)) => *n,
        _ => panic!("missing id"),
    };
    assert_eq!(id, 1);
    let score = match result.rows[0].get("_score") {
        Some(Value::Float(s)) => *s,
        _ => panic!("missing _score"),
    };
    assert!(score > 0.5, "{score}");
}

#[test]
fn deep_predict_unknown_model_errors() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE seeds (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let err = engine
        .sql("SELECT id FROM seeds WHERE deep_predict('missing')", &[])
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("missing"), "{msg}");
}
