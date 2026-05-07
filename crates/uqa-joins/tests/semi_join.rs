//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_semi_join.py` onto row-oriented joins.

use uqa_core::Value;
use uqa_joins::{anti_join, semi_join, JoinKey};
use uqa_sql::ResultRow;

fn row(pairs: &[(&str, Value)]) -> ResultRow {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

fn rows(doc_ids: &[i64]) -> Vec<ResultRow> {
    doc_ids
        .iter()
        .map(|id| row(&[("id", Value::Int(*id)), ("score", Value::Float(*id as f64))]))
        .collect()
}

fn ids(rows: &[ResultRow]) -> Vec<i64> {
    rows.iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(id)) => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn basic_semi_join() {
    let left = rows(&[1, 2, 3]);
    let right = rows(&[2, 3, 4]);
    let result = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(ids(&result), vec![2, 3]);
}

#[test]
fn basic_anti_join() {
    let left = rows(&[1, 2, 3]);
    let right = rows(&[2, 3, 4]);
    let result = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(ids(&result), vec![1]);
}

#[test]
fn semi_join_with_custom_condition() {
    let left = vec![
        row(&[("id", Value::Int(1)), ("dept", Value::Str("eng".into()))]),
        row(&[("id", Value::Int(2)), ("dept", Value::Str("sales".into()))]),
        row(&[("id", Value::Int(3)), ("dept", Value::Str("hr".into()))]),
    ];
    let right = vec![
        row(&[
            ("id", Value::Int(10)),
            ("department", Value::Str("eng".into())),
        ]),
        row(&[
            ("id", Value::Int(11)),
            ("department", Value::Str("sales".into())),
        ]),
    ];
    let result = semi_join(
        &left,
        &right,
        |r| r.get("dept").map(JoinKey::new),
        |r| r.get("department").map(JoinKey::new),
    );
    assert_eq!(ids(&result), vec![1, 2]);
}

#[test]
fn anti_join_with_custom_condition() {
    let left = vec![
        row(&[("id", Value::Int(1)), ("dept", Value::Str("eng".into()))]),
        row(&[("id", Value::Int(2)), ("dept", Value::Str("sales".into()))]),
        row(&[("id", Value::Int(3)), ("dept", Value::Str("hr".into()))]),
    ];
    let right = vec![
        row(&[("department", Value::Str("eng".into()))]),
        row(&[("department", Value::Str("sales".into()))]),
    ];
    let result = anti_join(
        &left,
        &right,
        |r| r.get("dept").map(JoinKey::new),
        |r| r.get("department").map(JoinKey::new),
    );
    assert_eq!(ids(&result), vec![3]);
}

#[test]
fn empty_left() {
    let left = Vec::new();
    let right = rows(&[1, 2]);
    let semi = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let anti = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert!(semi.is_empty());
    assert!(anti.is_empty());
}

#[test]
fn empty_right() {
    let left = rows(&[1, 2, 3]);
    let right = Vec::new();
    let semi = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let anti = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert!(semi.is_empty());
    assert_eq!(ids(&anti), vec![1, 2, 3]);
}

#[test]
fn no_overlap() {
    let left = rows(&[1, 2, 3]);
    let right = rows(&[4, 5, 6]);
    let semi = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let anti = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert!(semi.is_empty());
    assert_eq!(ids(&anti), vec![1, 2, 3]);
}

#[test]
fn not_commutative_payload_identity() {
    let left = vec![row(&[
        ("id", Value::Int(1)),
        ("score", Value::Float(0.1)),
        ("src", Value::Str("left".into())),
    ])];
    let right = vec![row(&[
        ("id", Value::Int(1)),
        ("score", Value::Float(0.9)),
        ("src", Value::Str("right".into())),
    ])];
    let left_right = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let right_left = semi_join(
        &right,
        &left,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(left_right[0]["src"], Value::Str("left".into()));
    assert_eq!(right_left[0]["src"], Value::Str("right".into()));
    assert_ne!(left_right[0]["score"], right_left[0]["score"]);
}

#[test]
fn payload_preservation() {
    let left = vec![
        row(&[
            ("id", Value::Int(1)),
            ("score", Value::Float(0.95)),
            ("name", Value::Str("Alice".into())),
            ("role", Value::Str("admin".into())),
        ]),
        row(&[
            ("id", Value::Int(2)),
            ("score", Value::Float(0.50)),
            ("name", Value::Str("Bob".into())),
            ("role", Value::Str("user".into())),
        ]),
        row(&[
            ("id", Value::Int(3)),
            ("score", Value::Float(0.10)),
            ("name", Value::Str("Charlie".into())),
            ("role", Value::Str("guest".into())),
        ]),
    ];
    let right = vec![row(&[("id", Value::Int(2))]), row(&[("id", Value::Int(3))])];
    let result = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(result.len(), 2);
    assert_eq!(result[0]["id"], Value::Int(2));
    assert_eq!(result[0]["score"], Value::Float(0.50));
    assert_eq!(result[0]["name"], Value::Str("Bob".into()));
    assert_eq!(result[0]["role"], Value::Str("user".into()));
    assert_eq!(result[1]["id"], Value::Int(3));
    assert_eq!(result[1]["score"], Value::Float(0.10));
    assert_eq!(result[1]["name"], Value::Str("Charlie".into()));
    assert_eq!(result[1]["role"], Value::Str("guest".into()));
}

#[test]
fn semi_and_anti_are_complementary() {
    let left = rows(&[1, 2, 3, 4, 5]);
    let right = rows(&[2, 4]);
    let semi = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let anti = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(ids(&semi), vec![2, 4]);
    assert_eq!(ids(&anti), vec![1, 3, 5]);
}

#[test]
fn full_overlap() {
    let left = rows(&[1, 2, 3]);
    let right = rows(&[1, 2, 3, 4, 5]);
    let semi = semi_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    let anti = anti_join(
        &left,
        &right,
        |r| r.get("id").map(JoinKey::new),
        |r| r.get("id").map(JoinKey::new),
    );
    assert_eq!(ids(&semi), vec![1, 2, 3]);
    assert!(anti.is_empty());
}
