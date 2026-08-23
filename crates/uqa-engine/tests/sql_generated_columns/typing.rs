//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column assignment and built-in type binding.

use super::*;

#[test]
fn generated_columns_apply_postgresql_assignment_casts() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_temporal_casts (
                 source_date DATE,
                 generated_timestamp TIMESTAMP GENERATED ALWAYS AS (source_date) STORED,
                 source_timestamp TIMESTAMP,
                 generated_date DATE GENERATED ALWAYS AS (source_timestamp) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_temporal_casts (source_date, source_timestamp)
             VALUES (DATE '2020-01-02', TIMESTAMP '2020-01-03 04:05:06')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT generated_timestamp::text AS generated_timestamp, generated_date::text AS generated_date
             FROM generated_temporal_casts",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["generated_timestamp"],
        Value::Str("2020-01-02 00:00:00".into())
    );
    assert_eq!(
        result.rows[0]["generated_date"],
        Value::Str("2020-01-03".into())
    );

    let error = engine
        .sql(
            "CREATE TABLE generated_uuid_rejects_text (
                 source TEXT,
                 generated UUID GENERATED ALWAYS AS (source) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("uuid"));
}

#[test]
fn generated_columns_accept_immutable_uuid_extraction_functions() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_uuid_extraction (
                 source UUID,
                 version SMALLINT GENERATED ALWAYS AS (uuid_extract_version(source)) STORED,
                 extracted_at TIMESTAMPTZ GENERATED ALWAYS AS (uuid_extract_timestamp(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_uuid_extraction (source) VALUES ('00000000-0001-7000-8000-000000000000')",
            &[],
        )
        .unwrap();
    let row = engine
        .sql(
            "SELECT version, extracted_at FROM generated_uuid_extraction",
            &[],
        )
        .unwrap()
        .rows
        .pop()
        .unwrap();
    assert_eq!(row["version"], Value::Int(7));
    assert_eq!(
        row["extracted_at"],
        Value::Temporal(uqa_core::TemporalValue::TimestampTz { micros: 1_000 })
    );

    let error = engine
        .sql(
            "CREATE TABLE generated_bad_uuid_extraction (
                 source TEXT,
                 version SMALLINT GENERATED ALWAYS AS (uuid_extract_version(source)) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function uuid_extract_version(text) does not exist"
    );
}

#[test]
fn generated_columns_reject_nonexistent_builtin_signatures() {
    let engine = Engine::new();
    for expression in [
        "cardinality(source, 1)",
        "array_reverse(source, true)",
        "array_remove(source, 1, 2)",
    ] {
        let sql = format!(
            "CREATE TABLE generated_bad_array_signature (source INTEGER[], generated INTEGER GENERATED ALWAYS AS ({expression}) STORED)"
        );
        assert!(engine.sql(&sql, &[]).is_err(), "{expression}");
    }
    assert!(engine
        .sql(
            "CREATE TABLE generated_bad_justify_hours (source DATE, generated INTERVAL GENERATED ALWAYS AS (justify_hours(source)) STORED)",
            &[],
        )
        .is_err());
    assert!(engine
        .sql(
            "CREATE TABLE generated_bad_make_timestamp (source INTEGER, generated TIMESTAMP GENERATED ALWAYS AS (make_timestamp(2020, 1, 1, 0, 0, 0, source)) STORED)",
            &[],
        )
        .is_err());
}

#[test]
fn generated_expressions_preserve_declared_integer_widths() {
    let engine = Engine::new();
    for kind in ["VIRTUAL", "STORED"] {
        let table = format!("generated_width_{}", kind.to_ascii_lowercase());
        engine
            .sql(
                &format!(
                    "CREATE TABLE {table} (source SMALLINT, bytes BYTEA GENERATED ALWAYS AS (source::bytea) {kind})"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} (source) VALUES (-1)"), &[])
            .unwrap();
        let result = engine
            .sql(&format!("SELECT bytes FROM {table}"), &[])
            .unwrap();
        assert_eq!(
            result.rows[0].get("bytes"),
            Some(&Value::Bytes(vec![0xff, 0xff]))
        );
    }
}
