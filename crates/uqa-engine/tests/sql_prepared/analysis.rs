//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation-time inference, validation, and fixed result descriptors.

use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/prepared_analysis_oracle.expected.json");

#[test]
fn prepared_analysis_matches_postgresql_memory() {
    super::parameters::verify_parameters(&Engine::new(), ORACLE);
}

#[test]
fn prepared_analysis_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("prepared-analysis.db")).unwrap();
    super::parameters::verify_parameters(&engine, ORACLE);
}

#[test]
fn prepared_analysis_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    super::parameters::verify_parameters(&engine, ORACLE);
}

#[test]
fn static_cast_analysis_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!("../../../../tests/parity/pg18/static_cast_analysis_oracle.expected.json"),
    );
}

#[test]
fn ordered_parameter_inference_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!(
            "../../../../tests/parity/pg18/ordered_parameter_inference_oracle.expected.json"
        ),
    );
}

#[test]
fn static_operator_analysis_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!("../../../../tests/parity/pg18/static_operator_analysis_oracle.expected.json"),
    );
}

#[test]
fn prepared_signatures_and_metadata_match_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!(
            "../../../../tests/parity/pg18/prepared_signatures_metadata_oracle.expected.json"
        ),
    );
}

#[test]
fn callback_registration_preserves_prepared_result_schema_validation() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE callback_result (id integer)", &[])
        .unwrap();
    engine
        .sql(
            "PREPARE callback_result AS SELECT * FROM callback_result",
            &[],
        )
        .unwrap();
    engine.sql("EXECUTE callback_result", &[]).unwrap();
    engine
        .sql("ALTER TABLE callback_result ADD COLUMN label text", &[])
        .unwrap();
    engine
        .register_scalar_function("unrelated_callback", |_: &[uqa_core::Value]| {
            Ok(uqa_core::Value::Int(1))
        })
        .unwrap();
    let error = engine.sql("EXECUTE callback_result", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(matches!(error, uqa_sql::SQLError::Routine { message, .. }
        if message == "cached plan must not change result type"));
}
