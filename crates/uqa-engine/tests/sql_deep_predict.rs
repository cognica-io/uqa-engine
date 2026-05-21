//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `deep_predict` SQL function and engine
//! deep-model persistence.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

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
fn deep_learn_sql_trains_and_persists_model_from_table() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE train (id INTEGER PRIMARY KEY, features REAL[], label INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO train (id, features, label) VALUES
             (1, ARRAY[2.0, 0.0], 0),
             (2, ARRAY[3.0, 0.0], 0),
             (3, ARRAY[0.0, 2.0], 1),
             (4, ARRAY[0.0, 3.0], 1)",
            &[],
        )
        .unwrap();

    let result = engine
        .sql("SELECT deep_learn('trained', 'train') AS report", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert!(engine.load_model("trained").is_some());

    let scores = engine
        .deep_predict_features("trained", &[(10, vec![4.0, 0.0]), (11, vec![0.0, 4.0])])
        .unwrap();
    assert_eq!(scores.len(), 2);
    assert!(scores.iter().all(|(_, score)| *score > 0.5), "{scores:?}");
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
        .sql(
            "CREATE TABLE seeds (id INTEGER PRIMARY KEY, status TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO seeds (id, status) VALUES (1, 'indexed')", &[])
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
fn deep_predict_combines_with_relational_filter() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE seeds (id INTEGER PRIMARY KEY, status TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO seeds (id, status) VALUES (1, 'indexed'), (2, 'draft')",
            &[],
        )
        .unwrap();
    let model = linear_classifier_model();
    engine.save_model("clf", &model).unwrap();

    let result = engine
        .sql(
            "SELECT id, _score FROM seeds WHERE deep_predict('clf') AND status = 'indexed'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let id = match result.rows[0].get("id") {
        Some(Value::Int(n)) => *n,
        _ => panic!("missing id"),
    };
    assert_eq!(id, 1);
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
