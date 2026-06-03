//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL surface coverage for predicate WHERE clauses (Boolean /
//! comparison / BETWEEN / IN / IS NULL) and aggregate functions with GROUP BY.

use uqa_core::Value;
use uqa_engine::Engine;

fn corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, year INTEGER, genre TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO books (id, title, year, genre) VALUES \
         (1, 'rust in action', 2021, 'tech'), \
         (2, 'programming rust', 2024, 'tech'), \
         (3, 'why rust is great', 2022, 'tech'), \
         (4, 'small gods', 1992, 'fiction'), \
         (5, 'thud', 2005, 'fiction'), \
         (6, 'dune', 1965, 'fiction'), \
         (7, 'the c programming language', 1988, 'tech')",
        &[],
    )
    .unwrap();
    eng
}

fn ints(rows: &[uqa_engine::SQLResult], col: &str) -> Vec<i64> {
    rows.iter()
        .flat_map(|r| r.rows.iter())
        .filter_map(|row| row.get(col).cloned())
        .filter_map(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .collect()
}

#[test]
fn comparison_where_filters_rows() {
    let eng = corpus();
    let r = eng
        .sql("SELECT id FROM books WHERE year > 2000 ORDER BY id", &[])
        .unwrap();
    let ids = ints(&[r], "id");
    assert_eq!(ids, vec![1, 2, 3, 5]);
}

#[test]
fn boolean_combination_filters_rows() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT id FROM books \
             WHERE year >= 2000 AND genre = 'tech' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids = ints(&[r], "id");
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn between_in_and_not_null_filters() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT id FROM books WHERE year BETWEEN 1985 AND 2005 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&[r], "id"), vec![4, 5, 7]);

    let r = eng
        .sql(
            "SELECT id FROM books WHERE genre IN ('fiction') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&[r], "id"), vec![4, 5, 6]);

    let r = eng
        .sql(
            "SELECT id FROM books WHERE title IS NOT NULL ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&[r], "id").len(), 7);
}

#[test]
fn count_star_with_filter() {
    let eng = corpus();
    let r = eng
        .sql("SELECT count(*) AS n FROM books WHERE genre = 'tech'", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(4)));
}

#[test]
fn group_by_genre_returns_one_row_per_group() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT genre, count(*) AS n FROM books GROUP BY genre ORDER BY genre",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].get("genre"), Some(&Value::Str("fiction".into())));
    assert_eq!(r.rows[0].get("n"), Some(&Value::Int(3)));
    assert_eq!(r.rows[1].get("genre"), Some(&Value::Str("tech".into())));
    assert_eq!(r.rows[1].get("n"), Some(&Value::Int(4)));
}

#[test]
fn min_max_avg_aggregates() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT min(year) AS mn, max(year) AS mx, avg(year) AS av FROM books",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("mn"), Some(&Value::Int(1965)));
    assert_eq!(r.rows[0].get("mx"), Some(&Value::Int(2024)));
    match r.rows[0].get("av") {
        Some(Value::Float(f)) => {
            // (2021 + 2024 + 2022 + 1992 + 2005 + 1965 + 1988) / 7
            let expected = (2021.0 + 2024.0 + 2022.0 + 1992.0 + 2005.0 + 1965.0 + 1988.0) / 7.0;
            assert!((f - expected).abs() < 1e-9, "got {f}, expected {expected}");
        }
        other => panic!("expected Float avg, got {other:?}"),
    }
}
