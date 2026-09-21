//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal index keys preserve absent reads and the predicate's future matching values.

use super::*;
use uqa_core::{Predicate, Value};

fn tables(seed: &Session, ty: &str) {
    for table in ["left_clock", "right_clock"] {
        seed.sql(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, k {ty} UNIQUE, payload INTEGER)"
        ));
        seed.sql(&format!("CREATE INDEX {table}_k ON {table} (k)"));
    }
}

#[test]
fn temporal_null_indexes_observe_only_matching_insertions() {
    for predicate in [Predicate::IsNull, Predicate::IsNotNull] {
        for value in ["NULL", "TIMESTAMP '2024-01-01 12:00:00'"] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                tables(&seed, "TIMESTAMP(6)");
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                assert_eq!(index_only(&a, "left_clock", &predicate), 0);
                assert_eq!(index_only(&b, "right_clock", &predicate), 0);
                a.sql(&format!("INSERT INTO right_clock VALUES (1, {value}, 0)"));
                b.sql(&format!("INSERT INTO left_clock VALUES (1, {value}, 0)"));
                let matches = matches!(predicate, Predicate::IsNull) == (value == "NULL");
                finish(&a, &b, matches);
            }
        }
    }
}

#[test]
fn empty_temporal_text_ranges_retain_future_matches() {
    let predicate = Predicate::Between {
        low: Value::Str("2024-01-01 00:00:00".into()),
        high: Value::Str("2024-01-31 23:59:59".into()),
    };
    for (value, conflict) in [
        ("2024-01-15 12:00:00", true),
        ("2024-02-01 00:00:00", false),
    ] {
        let (_directory, sessions) = fixtures();
        for seed in sessions {
            tables(&seed, "TIMESTAMP");
            let a = seed.sibling();
            let b = seed.sibling();
            a.begin();
            b.begin();
            assert_eq!(index_only(&a, "left_clock", &predicate), 0);
            assert_eq!(index_only(&b, "right_clock", &predicate), 0);
            a.sql(&format!(
                "INSERT INTO right_clock VALUES (1, TIMESTAMP '{value}', 0)"
            ));
            b.sql(&format!(
                "INSERT INTO left_clock VALUES (1, TIMESTAMP '{value}', 0)"
            ));
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn absent_temporal_unique_updates_keep_native_equality() {
    for (ty, probe, same, different) in [
        (
            "DATE",
            "DATE '2024-01-01'",
            "DATE '2024-01-01'",
            "DATE '2024-01-02'",
        ),
        (
            "TIME(6)",
            "TIME '12:00:00.123456'",
            "TIME '12:00:00.123456'",
            "TIME '12:00:00.123457'",
        ),
        (
            "TIMETZ(6)",
            "TIMETZ '12:00:00+00'",
            "TIMETZ '12:00:00+00'",
            "TIMETZ '14:00:00+00'",
        ),
        (
            "TIMESTAMP(6)",
            "TIMESTAMP '2024-01-01 12:00:00'",
            "TIMESTAMP '2024-01-01 12:00:00'",
            "TIMESTAMP '2024-01-02 12:00:00'",
        ),
        (
            "TIMESTAMPTZ(6)",
            "TIMESTAMPTZ '2024-01-01 12:00:00+00'",
            "TIMESTAMPTZ '2024-01-01 13:00:00+01'",
            "TIMESTAMPTZ '2024-01-02 12:00:00+00'",
        ),
        (
            "INTERVAL",
            "INTERVAL '1 month'",
            "INTERVAL '30 days'",
            "INTERVAL '31 days'",
        ),
    ] {
        for (value, conflict) in [(same, true), (different, false)] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                tables(&seed, ty);
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                for (session, table) in [(&a, "left_clock"), (&b, "right_clock")] {
                    assert_eq!(
                        session
                            .sql(&format!("UPDATE {table} SET payload = 1 WHERE k = {probe}"))
                            .affected_rows,
                        0
                    );
                }
                a.sql(&format!("INSERT INTO right_clock VALUES (1, {value}, 0)"));
                b.sql(&format!("INSERT INTO left_clock VALUES (1, {value}, 0)"));
                finish(&a, &b, conflict);
            }
        }
    }
}
