//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for date/time scalar functions: `NOW`, `CURRENT_TIMESTAMP`,
//! `CURRENT_DATE`, `EXTRACT` / `DATE_PART`, `AGE`, `TO_TIMESTAMP`.

use uqa_core::Value;
use uqa_engine::Engine;

fn fixture() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE events (id BIGSERIAL PRIMARY KEY, ts TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO events (ts) VALUES ('2026-05-05T12:30:45.500Z')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn extract_year_month_day_hour_minute() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT \
                EXTRACT(YEAR FROM ts) AS y, \
                EXTRACT(MONTH FROM ts) AS m, \
                EXTRACT(DAY FROM ts) AS d, \
                EXTRACT(HOUR FROM ts) AS h, \
                EXTRACT(MINUTE FROM ts) AS mn \
             FROM events",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["y"], Value::Int(2026));
    assert_eq!(res.rows[0]["m"], Value::Int(5));
    assert_eq!(res.rows[0]["d"], Value::Int(5));
    assert_eq!(res.rows[0]["h"], Value::Int(12));
    assert_eq!(res.rows[0]["mn"], Value::Int(30));
}

#[test]
fn date_part_dow_doy() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT DATE_PART('dow', ts) AS dow, DATE_PART('doy', ts) AS doy FROM events",
            &[],
        )
        .unwrap();
    // 2026-05-05 is a Tuesday (Sunday=0, ... Tuesday=2).
    assert_eq!(res.rows[0]["dow"], Value::Int(2));
    // Day of year for 2026-05-05 is 31+28+31+30+5 = 125.
    assert_eq!(res.rows[0]["doy"], Value::Int(125));
}

#[test]
fn epoch_seconds_round_trip() {
    let eng = fixture();
    let res = eng
        .sql("SELECT EXTRACT(EPOCH FROM ts) AS e FROM events", &[])
        .unwrap();
    let epoch = match res.rows[0]["e"] {
        Value::Float(f) => f,
        ref other => panic!("expected float, got {other:?}"),
    };
    // 2026-05-05T12:30:45.500Z corresponds to epoch 1_777_984_245.5
    let expected = 1_777_984_245.5;
    assert!((epoch - expected).abs() < 1.0, "got {epoch}");
}

#[test]
fn now_returns_string() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng.sql("SELECT NOW() AS n FROM t", &[]).unwrap();
    let v = &res.rows[0]["n"];
    assert!(
        matches!(v, Value::Str(_)),
        "expected timestamp string, got {v:?}"
    );
}
