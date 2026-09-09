//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{TemporalValue, Value};
use uqa_engine::Engine;

#[test]
fn interval_fields_and_precision_preserve_values_and_metadata_after_reopen() {
    fn check(engine: &Engine) {
        let result = engine
            .sql(
                "SELECT wide, years, minutes, fractional FROM interval_precision",
                &[],
            )
            .unwrap();
        for (index, expected) in [
            "1 year 2 mons 3 days 04:05:06.79",
            "1 year",
            "1 year 2 mons 3 days 04:05:00",
            "1 year 2 mons 3 days 04:05:06.79",
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                uqa_sql::expr::value_to_string(result.rows[0].get(&result.columns[index]).unwrap()),
                expected
            );
        }
        let result = engine.sql("SELECT datetime_precision, interval_type FROM information_schema.columns WHERE table_name = 'interval_precision' ORDER BY ordinal_position", &[]).unwrap();
        for (index, (precision, fields)) in [
            (3, None),
            (6, Some("YEAR")),
            (6, Some("HOUR TO MINUTE")),
            (3, Some("DAY TO SECOND(3)")),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                result.rows[index].get("datetime_precision"),
                Some(&Value::Int(precision))
            );
            assert_eq!(
                result.rows[index].get("interval_type"),
                Some(&fields.map_or(Value::Null, |fields| Value::Str(fields.into())))
            );
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interval-precision.sqlite3");
    for engine in [Engine::new(), Engine::open(&path).unwrap()] {
        engine.sql("CREATE TABLE interval_precision (wide interval(3), years interval year, minutes interval hour to minute, fractional interval day to second(3))", &[]).unwrap();
        engine.sql("INSERT INTO interval_precision SELECT value, value, value, value FROM (VALUES ('1 year 2 mons 3 days 04:05:06.7895'::interval)) AS inputs(value)", &[]).unwrap();
        check(&engine);
    }
    check(&Engine::open(&path).unwrap());
}

#[test]
fn temporal_precision_preserves_rounding_and_catalog_metadata_after_reopen() {
    use uqa_engine::sql::postgres_result_type;
    use uqa_sql::ast::ColumnType;

    fn check(engine: &Engine) {
        let result = engine
            .sql("SELECT t, tz, ts, tstz FROM temporal_precision", &[])
            .unwrap();
        assert_eq!(
            result.column_types,
            vec![
                Some(ColumnType::TimePrecision(3)),
                Some(ColumnType::TimeTzPrecision(3)),
                Some(ColumnType::TimestampPrecision(3)),
                Some(ColumnType::TimestampTzPrecision(3)),
            ]
        );
        for (position, expected) in [
            "24:00:00",
            "24:00:00+09",
            "1999-12-31 23:59:59.999",
            "2000-01-01 00:00:00.001+00",
        ]
        .into_iter()
        .enumerate()
        {
            let value = result.rows[0].get(&result.columns[position]).unwrap();
            assert_eq!(uqa_sql::expr::value_to_string(value), expected);
            let ty = result.column_types[position].as_ref().unwrap();
            assert_eq!(postgres_result_type(ty).type_modifier, 3);
        }
        let precision = engine.sql("SELECT datetime_precision FROM information_schema.columns WHERE table_name = 'temporal_precision' ORDER BY ordinal_position", &[]).unwrap();
        assert_eq!(precision.rows.len(), 4);
        assert!(precision
            .rows
            .iter()
            .all(|row| row.get("datetime_precision") == Some(&Value::Int(3))));
        let catalog = engine.sql("SELECT atttypmod FROM pg_attribute WHERE attrelid = 'temporal_precision'::regclass AND attnum > 0 ORDER BY attnum", &[]).unwrap();
        assert_eq!(catalog.rows.len(), 4);
        assert!(catalog
            .rows
            .iter()
            .all(|row| row.get("atttypmod") == Some(&Value::Int(3))));
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporal-precision.sqlite3");
    for engine in [Engine::new(), Engine::open(&path).unwrap()] {
        engine.sql("CREATE TABLE temporal_precision (t time(3), tz time(3) with time zone, ts timestamp(3), tstz timestamp(3) with time zone)", &[]).unwrap();
        engine.sql("INSERT INTO temporal_precision VALUES ('23:59:59.9995', '23:59:59.9995+09', '1999-12-31 23:59:59.9995', '2000-01-01 00:00:00.0005+00')", &[]).unwrap();
        check(&engine);
    }
    check(&Engine::open(&path).unwrap());
}

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

    let doc = engine
        .get_document("events", 1)
        .unwrap()
        .expect("temporal event row");
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
    let doc = engine
        .get_document("inputs", 1)
        .unwrap()
        .expect("timestamp input row");
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
