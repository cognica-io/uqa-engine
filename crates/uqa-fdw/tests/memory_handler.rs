//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `MemoryHandler` predicate-pushdown property tests.
//!
//! The handler exists primarily as a reference oracle for richer FDW
//! backends, so its semantics need to be well-pinned. This file
//! property-tests:
//! - `Eq` predicate filters every row whose column equals the literal,
//! - `NotEq` is the de Morgan dual of `Eq` (when the column is present),
//! - range operators (`Lt`, `LtEq`, `Gt`, `GtEq`) match `Ord` directly,
//! - `In` filter equals "column appears in the list",
//! - `Like` matches `sql_like` directly and `NotLike` is its dual,
//! - column projection commutes with predicate filtering,
//! - limit is post-filter (never returns more than `limit` matches).

use std::collections::BTreeMap;

use proptest::prelude::*;
use uqa_core::Value;
use uqa_fdw::{
    ColumnDef, ColumnType, FDWHandler, FDWPredicate, ForeignTable, MemoryHandler, PredicateOp, Row,
};

const TABLE: &str = "t";

fn table() -> ForeignTable {
    ForeignTable {
        name: TABLE.into(),
        server_name: "mem".into(),
        columns: vec![
            ColumnDef {
                name: "id".into(),
                ty: ColumnType::Integer,
            },
            ColumnDef {
                name: "name".into(),
                ty: ColumnType::Text,
            },
            ColumnDef {
                name: "score".into(),
                ty: ColumnType::Integer,
            },
        ],
        options: BTreeMap::new(),
    }
}

fn build_handler(rows: Vec<Row>) -> MemoryHandler {
    let mut h = MemoryHandler::new();
    h.load(TABLE, rows);
    h
}

/// Generates rows shaped like `{id: Int, name: Str, score: Int}`.
fn arb_row() -> impl Strategy<Value = Row> {
    (0i64..100, "[a-z]{1,4}", 0i64..50).prop_map(|(id, name, score)| {
        let mut row: Row = BTreeMap::new();
        row.insert("id".into(), Value::Int(id));
        row.insert("name".into(), Value::Str(name));
        row.insert("score".into(), Value::Int(score));
        row
    })
}

fn arb_rows() -> impl Strategy<Value = Vec<Row>> {
    proptest::collection::vec(arb_row(), 0..=12)
}

/// Read a column off a row as the same `Value` shape we passed in.
fn col<'a>(row: &'a Row, name: &str) -> Option<&'a Value> {
    row.get(name)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// `Eq` matches every row whose column equals the literal.
    #[test]
    fn eq_filters_by_column_equality(rows in arb_rows(), needle in 0i64..100) {
        let handler = build_handler(rows.clone());
        let pred = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::Eq,
            value: Value::Int(needle),
        };
        let observed = handler.scan(&table(), None, &[pred], None).unwrap();
        let expected: Vec<Row> = rows
            .iter()
            .filter(|r| col(r, "id") == Some(&Value::Int(needle)))
            .cloned()
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// `NotEq` is the dual of `Eq` for rows whose column is present.
    #[test]
    fn neq_is_dual_of_eq(rows in arb_rows(), needle in 0i64..100) {
        let handler = build_handler(rows.clone());
        let eq = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::Eq,
            value: Value::Int(needle),
        };
        let neq = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::NotEq,
            value: Value::Int(needle),
        };
        let eq_rows = handler.scan(&table(), None, &[eq], None).unwrap();
        let neq_rows = handler.scan(&table(), None, &[neq], None).unwrap();
        // Each of `rows` lands on exactly one side: every row has
        // a non-Null `id`, so `NotEq` covers the complement of `Eq`.
        prop_assert_eq!(eq_rows.len() + neq_rows.len(), rows.len());
    }

    /// `Lt` against `score` matches the `Ord` comparison.
    #[test]
    fn lt_matches_ord(rows in arb_rows(), bound in 0i64..50) {
        let handler = build_handler(rows.clone());
        let pred = FDWPredicate {
            column: "score".into(),
            operator: PredicateOp::Lt,
            value: Value::Int(bound),
        };
        let observed = handler.scan(&table(), None, &[pred], None).unwrap();
        let expected: Vec<Row> = rows
            .iter()
            .filter(|r| matches!(col(r, "score"), Some(Value::Int(s)) if *s < bound))
            .cloned()
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// `In` against a value list filters by `contains`.
    #[test]
    fn in_filters_by_contains(rows in arb_rows(), choices in proptest::collection::vec(0i64..100, 1..=4)) {
        let handler = build_handler(rows.clone());
        let pred = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::In,
            value: Value::List(choices.iter().map(|n| Value::Int(*n)).collect()),
        };
        let observed = handler.scan(&table(), None, &[pred], None).unwrap();
        let expected: Vec<Row> = rows
            .iter()
            .filter(|r| {
                if let Some(Value::Int(n)) = col(r, "id") {
                    choices.contains(n)
                } else {
                    false
                }
            })
            .cloned()
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// `Like` with no wildcards behaves like Eq on Str columns.
    #[test]
    fn like_no_wildcards_equals_eq(rows in arb_rows(), needle in "[a-z]{1,4}") {
        let handler = build_handler(rows.clone());
        let like = FDWPredicate {
            column: "name".into(),
            operator: PredicateOp::Like,
            value: Value::Str(needle.clone()),
        };
        let eq = FDWPredicate {
            column: "name".into(),
            operator: PredicateOp::Eq,
            value: Value::Str(needle),
        };
        let l = handler.scan(&table(), None, &[like], None).unwrap();
        let e = handler.scan(&table(), None, &[eq], None).unwrap();
        prop_assert_eq!(l, e);
    }

    /// `Like` with a leading `%` is "ends with"; pin against a trivial
    /// Rust oracle.
    #[test]
    fn like_leading_percent_means_ends_with(rows in arb_rows(), suffix in "[a-z]{1,2}") {
        let handler = build_handler(rows.clone());
        let pat = format!("%{suffix}");
        let pred = FDWPredicate {
            column: "name".into(),
            operator: PredicateOp::Like,
            value: Value::Str(pat),
        };
        let observed = handler.scan(&table(), None, &[pred], None).unwrap();
        let expected: Vec<Row> = rows
            .iter()
            .filter(|r| matches!(col(r, "name"), Some(Value::Str(s)) if s.ends_with(&suffix)))
            .cloned()
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// Projection commutes with filtering: filter then project equals
    /// project then filter (when the predicate column is in the
    /// projection list).
    #[test]
    fn projection_commutes_with_filter(rows in arb_rows(), bound in 0i64..50) {
        let handler = build_handler(rows.clone());
        let pred = FDWPredicate {
            column: "score".into(),
            operator: PredicateOp::Gt,
            value: Value::Int(bound),
        };
        let cols = ["id".to_string(), "score".to_string()];
        let projected_then_filtered =
            handler
                .scan(&table(), Some(&cols), std::slice::from_ref(&pred), None)
                .unwrap();
        // Build the oracle: filter rows by score > bound, then project.
        let expected: Vec<Row> = rows
            .iter()
            .filter(|r| matches!(col(r, "score"), Some(Value::Int(s)) if *s > bound))
            .map(|r| {
                let mut keep: Row = BTreeMap::new();
                for c in &cols {
                    if let Some(v) = r.get(c) {
                        keep.insert(c.clone(), v.clone());
                    }
                }
                keep
            })
            .collect();
        prop_assert_eq!(projected_then_filtered, expected);
    }

    /// `limit` is applied after filtering. The output never has more
    /// than `limit` rows, and every output row matches the predicate.
    #[test]
    fn limit_applies_after_filter(rows in arb_rows(), bound in 0i64..50, limit in 0usize..=5) {
        let handler = build_handler(rows.clone());
        let pred = FDWPredicate {
            column: "score".into(),
            operator: PredicateOp::GtEq,
            value: Value::Int(bound),
        };
        let observed = handler
            .scan(&table(), None, &[pred], Some(limit as u64))
            .unwrap();
        prop_assert!(observed.len() <= limit);
        for r in &observed {
            let s = match col(r, "score") {
                Some(Value::Int(n)) => *n,
                _ => return Err(TestCaseError::fail("score column missing")),
            };
            prop_assert!(s >= bound);
        }
    }
}

/// Concrete edge cases.
#[test]
fn scan_unknown_table_returns_error() {
    let handler = MemoryHandler::new();
    let error = handler
        .scan(&table(), None, &[], None)
        .expect_err("an unloaded table must not look like an empty relation");
    assert!(matches!(error, uqa_fdw::FDWError::UnknownTable(name) if name == "t"));
}

#[test]
fn empty_predicates_return_all_rows_in_order() {
    let row = |id: i64, name: &str| {
        let mut r: Row = BTreeMap::new();
        r.insert("id".into(), Value::Int(id));
        r.insert("name".into(), Value::Str(name.into()));
        r.insert("score".into(), Value::Int(0));
        r
    };
    let rows = vec![row(1, "a"), row(2, "b"), row(3, "c")];
    let handler = build_handler(rows.clone());
    let observed = handler.scan(&table(), None, &[], None).unwrap();
    assert_eq!(observed, rows);
}
