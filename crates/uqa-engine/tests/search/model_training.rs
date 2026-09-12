//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_ml::{LearnOptions, TrainingSet};

#[test]
fn training_apis_share_native_execution_and_preserve_sqlite_model_transactions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("training.db");
    let engine = Engine::open(&path).unwrap();
    engine
        .sql(
            "CREATE TABLE training (id INTEGER PRIMARY KEY, features REAL[], label INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO training VALUES (1, ARRAY[2.0], 0), (2, ARRAY[7.0], 1)",
            &[],
        )
        .unwrap();
    let json = r#"{"examples":[{"features":[2.0],"label":0},{"features":[7.0],"label":1}]}"#;
    let set: TrainingSet = serde_json::from_str(json).unwrap();
    let expected = engine
        .deep_learn("typed", &set, &LearnOptions::default())
        .unwrap();
    assert_eq!(
        engine
            .deep_learn_json("json", json, &LearnOptions::default())
            .unwrap(),
        expected
    );
    assert_eq!(
        engine
            .deep_learn_table("table", "training", &LearnOptions::default())
            .unwrap(),
        expected
    );
    let result = engine
        .sql("SELECT deep_learn('sql', 'training') AS report", &[])
        .unwrap();
    let Some(Value::Map(report)) = result.rows[0].get("report") else {
        panic!("training report")
    };
    assert_eq!(report.get("examples"), Some(&Value::Int(2)));
    assert_eq!(report.get("feature_dimensions"), Some(&Value::Int(1)));
    assert_eq!(report.get("class_count"), Some(&Value::Int(2)));
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .deep_learn_json("rolled_back", json, &LearnOptions::default())
        .unwrap();
    assert!(engine.load_model("rolled_back").unwrap().is_some());
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(engine.load_model("rolled_back").unwrap().is_none());
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    for name in ["typed", "json", "table", "sql"] {
        assert_eq!(
            reopened.load_model(name).unwrap(),
            Some(expected.model.clone())
        );
    }
    assert!(reopened.load_model("rolled_back").unwrap().is_none());
}

#[test]
fn training_projects_generated_labels_without_evaluating_unrelated_virtual_columns() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE training (id INTEGER PRIMARY KEY, features REAL[], source INTEGER, label INTEGER GENERATED ALWAYS AS (source - 1), unrelated INTEGER GENERATED ALWAYS AS (1 / (source - source)))", &[]).unwrap();
    engine.sql("INSERT INTO training (id, features, source) VALUES (1, ARRAY[2.0], 1), (2, ARRAY[7.0], 2)", &[]).unwrap();
    let result = engine
        .deep_learn_table("projected", "training", &LearnOptions::default())
        .unwrap();
    assert_eq!(result.report.examples, 2);
    assert_eq!(result.report.class_count, 2);
    assert_eq!(engine.load_model("projected").unwrap(), Some(result.model));
    assert!(engine.sql("SELECT unrelated FROM training", &[]).is_err());
}
