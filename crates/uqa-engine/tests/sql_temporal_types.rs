//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{TemporalValue, Value};
use uqa_engine::Engine;

#[test]
fn temporal_columns_store_typed_values_and_compare_by_time_key() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE events (
                id INTEGER PRIMARY KEY,
                event_date DATE,
                start_time TIME,
                start_time_tz TIME WITH TIME ZONE,
                created_at TIMESTAMP WITHOUT TIME ZONE,
                observed_at TIMESTAMP WITH TIME ZONE
            )",
            &[],
        )
        .unwrap();

    engine
        .sql(
            "INSERT INTO events
             (id, event_date, start_time, start_time_tz, created_at, observed_at)
             VALUES
             (1, '2026-05-14', '09:30:00', '09:30:00+09:00',
              '2026-05-14 09:30:00', '2026-05-14T00:30:00Z'),
             (2, '2026-05-13', '10:00:00', '10:00:00+09:00',
              '2026-05-13 10:00:00', '2026-05-13T01:00:00Z')",
            &[],
        )
        .unwrap();

    let doc = engine.get_document("events", 1).unwrap();
    assert!(matches!(
        doc.get("event_date"),
        Some(Value::Temporal(TemporalValue::Date { .. }))
    ));
    assert!(matches!(
        doc.get("start_time"),
        Some(Value::Temporal(TemporalValue::Time { .. }))
    ));
    assert!(matches!(
        doc.get("start_time_tz"),
        Some(Value::Temporal(TemporalValue::TimeTz { .. }))
    ));
    assert!(matches!(
        doc.get("created_at"),
        Some(Value::Temporal(TemporalValue::Timestamp { .. }))
    ));
    assert!(matches!(
        doc.get("observed_at"),
        Some(Value::Temporal(TemporalValue::TimestampTz { .. }))
    ));

    let filtered = engine
        .sql(
            "SELECT id FROM events
             WHERE created_at >= '2026-05-14 00:00:00'
             ORDER BY created_at",
            &[],
        )
        .unwrap();
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(filtered.rows[0].get("id"), Some(&Value::Int(1)));

    let ordered = engine
        .sql("SELECT id FROM events ORDER BY observed_at", &[])
        .unwrap();
    assert_eq!(ordered.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(ordered.rows[1].get("id"), Some(&Value::Int(1)));
}

#[test]
fn timestamp_without_time_zone_accepts_now_default() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE inputs (
                id INTEGER PRIMARY KEY,
                created_at TIMESTAMP DEFAULT NOW()
            )",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO inputs (id) VALUES (1)", &[])
        .unwrap();
    let doc = engine.get_document("inputs", 1).unwrap();
    assert!(matches!(
        doc.get("created_at"),
        Some(Value::Temporal(TemporalValue::Timestamp { .. }))
    ));
}

#[test]
fn information_schema_reports_temporal_column_types() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE events (
                id INTEGER PRIMARY KEY,
                event_date DATE,
                created_at TIMESTAMP WITHOUT TIME ZONE,
                observed_at TIMESTAMP WITH TIME ZONE
            )",
            &[],
        )
        .unwrap();

    let rows = engine
        .sql(
            "SELECT column_name, data_type
             FROM information_schema.columns
             WHERE table_name = 'events'
             ORDER BY ordinal_position",
            &[],
        )
        .unwrap()
        .rows;
    let pairs = rows
        .iter()
        .map(|row| {
            let Some(Value::Str(name)) = row.get("column_name") else {
                panic!("missing column_name");
            };
            let Some(Value::Str(data_type)) = row.get("data_type") else {
                panic!("missing data_type");
            };
            (name.as_str(), data_type.as_str())
        })
        .collect::<Vec<_>>();
    assert!(pairs.contains(&("event_date", "date")));
    assert!(pairs.contains(&("created_at", "timestamp")));
    assert!(pairs.contains(&("observed_at", "timestamp with time zone")));
}
