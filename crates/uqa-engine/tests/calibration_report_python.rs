//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ports Engine calibration report cases from `uqa/tests/test_calibration.py`.

use uqa_engine::Engine;

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id SERIAL PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_docs_gin ON docs USING gin (content)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (content) VALUES
             ('machine learning algorithms'),
             ('deep learning neural networks'),
             ('database indexing structures'),
             ('search engine optimization')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn calibration_report_returns_struct() {
    let engine = engine_with_docs();
    let report = engine
        .calibration_report("docs", "content", "learning", &[1, 1, 0, 0])
        .unwrap();
    assert!(report.ece >= 0.0);
    assert!(report.brier >= 0.0);
}

#[test]
fn calibration_report_wrong_label_count() {
    let engine = engine_with_docs();
    let err = engine
        .calibration_report("docs", "content", "learning", &[1, 0])
        .unwrap_err()
        .to_string();
    assert!(err.contains("labels length"));
}

#[test]
fn calibration_report_nonexistent_table() {
    let engine = engine_with_docs();
    let err = engine
        .calibration_report("nonexistent", "content", "learning", &[1])
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown table") || err.contains("does not exist"));
}
