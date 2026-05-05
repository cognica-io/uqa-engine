//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign data wrappers for external sources.
//!
//! A [`ForeignServer`] captures the connection metadata for an external
//! source; a [`ForeignTable`] is a virtual table backed by such a
//! server. The [`FDWHandler`] trait drives the actual scan, and
//! [`MemoryHandler`] is the simplest concrete implementation: it
//! stores pre-loaded rows and applies projection, pushdown predicate
//! filtering, and a row limit before returning.

use std::collections::BTreeMap;

use uqa_core::Value;

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Integer,
    Real,
    Text,
    Bool,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct ForeignServer {
    pub name: String,
    /// Handler type — `"memory"`, `"duckdb_fdw"`, `"arrow_fdw"`, ...
    pub fdw_type: String,
    pub options: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ForeignTable {
    pub name: String,
    pub server_name: String,
    pub columns: Vec<ColumnDef>,
    pub options: BTreeMap<String, String>,
}

/// Pushdown comparison operator. Operators outside this set must be
/// evaluated above the wrapper; handlers may opt to ignore unknown
/// values rather than fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    In,
    Like,
    NotLike,
}

#[derive(Debug, Clone)]
pub struct FDWPredicate {
    pub column: String,
    pub operator: PredicateOp,
    /// Scalar literal for comparisons; a `Value::List` for `In`.
    pub value: Value,
}

pub type Row = BTreeMap<String, Value>;

pub trait FDWHandler: Send + Sync {
    fn scan(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<usize>,
    ) -> Vec<Row>;

    fn close(&self) {}
}

/// `MemoryHandler` keeps a flat row vector keyed on `ForeignTable::name`.
/// `scan` projects, filters, and limits in pure Rust — handy for tests
/// and as a baseline reference for richer backends.
#[derive(Debug, Default)]
pub struct MemoryHandler {
    tables: BTreeMap<String, Vec<Row>>,
}

impl MemoryHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(&mut self, table_name: impl Into<String>, rows: Vec<Row>) {
        self.tables.insert(table_name.into(), rows);
    }
}

impl FDWHandler for MemoryHandler {
    fn scan(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<usize>,
    ) -> Vec<Row> {
        let Some(rows) = self.tables.get(&table.name) else {
            return Vec::new();
        };
        let mut out: Vec<Row> = Vec::new();
        for row in rows {
            if let Some(cap) = limit {
                if out.len() >= cap {
                    break;
                }
            }
            if !predicates.iter().all(|p| eval_predicate(p, row)) {
                continue;
            }
            let projected = match columns {
                Some(cols) => {
                    let mut keep = Row::new();
                    for col in cols {
                        if let Some(v) = row.get(col) {
                            keep.insert(col.clone(), v.clone());
                        }
                    }
                    keep
                }
                None => row.clone(),
            };
            out.push(projected);
        }
        out
    }
}

fn eval_predicate(p: &FDWPredicate, row: &Row) -> bool {
    let lhs = row.get(&p.column);
    match p.operator {
        PredicateOp::Eq => lhs == Some(&p.value),
        PredicateOp::NotEq => lhs.is_some() && lhs != Some(&p.value),
        PredicateOp::Lt => lhs.is_some_and(|v| v < &p.value),
        PredicateOp::LtEq => lhs.is_some_and(|v| v <= &p.value),
        PredicateOp::Gt => lhs.is_some_and(|v| v > &p.value),
        PredicateOp::GtEq => lhs.is_some_and(|v| v >= &p.value),
        PredicateOp::In => match (&p.value, lhs) {
            (Value::List(items), Some(v)) => items.iter().any(|item| item == v),
            _ => false,
        },
        PredicateOp::Like => string_matches(lhs, &p.value),
        PredicateOp::NotLike => !string_matches(lhs, &p.value),
    }
}

fn string_matches(lhs: Option<&Value>, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Some(Value::Str(haystack)), Value::Str(pattern)) => sql_like(haystack, pattern),
        _ => false,
    }
}

/// SQL `LIKE` matcher with `%` (zero+ chars) and `_` (single char)
/// wildcards. Greedy backtracking; safe on non-ASCII because we work
/// in `char`s, not bytes.
fn sql_like(haystack: &str, pattern: &str) -> bool {
    let h: Vec<char> = haystack.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_match(&h, &p)
}

fn like_match(h: &[char], p: &[char]) -> bool {
    let mut hi = 0;
    let mut pi = 0;
    let mut star_h: Option<usize> = None;
    let mut star_p: Option<usize> = None;
    while hi < h.len() {
        match p.get(pi) {
            Some('%') => {
                star_p = Some(pi);
                star_h = Some(hi);
                pi += 1;
            }
            Some('_') => {
                hi += 1;
                pi += 1;
            }
            Some(c) if *c == h[hi] => {
                hi += 1;
                pi += 1;
            }
            _ => {
                if let (Some(sp), Some(sh)) = (star_p, star_h) {
                    pi = sp + 1;
                    star_h = Some(sh + 1);
                    hi = sh + 1;
                } else {
                    return false;
                }
            }
        }
    }
    while p.get(pi) == Some(&'%') {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Value)]) -> Row {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn books_table() -> ForeignTable {
        ForeignTable {
            name: "books".into(),
            server_name: "mem".into(),
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    ty: ColumnType::Integer,
                },
                ColumnDef {
                    name: "title".into(),
                    ty: ColumnType::Text,
                },
                ColumnDef {
                    name: "year".into(),
                    ty: ColumnType::Integer,
                },
            ],
            options: BTreeMap::new(),
        }
    }

    #[test]
    fn memory_handler_returns_all_rows_unfiltered() {
        let mut handler = MemoryHandler::new();
        handler.load(
            "books",
            vec![
                row(&[
                    ("id", Value::Int(1)),
                    ("title", Value::Str("a".into())),
                    ("year", Value::Int(2024)),
                ]),
                row(&[
                    ("id", Value::Int(2)),
                    ("title", Value::Str("b".into())),
                    ("year", Value::Int(2023)),
                ]),
            ],
        );
        let rows = handler.scan(&books_table(), None, &[], None);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn pushdown_predicate_filters_rows() {
        let mut handler = MemoryHandler::new();
        handler.load(
            "books",
            vec![
                row(&[("id", Value::Int(1)), ("year", Value::Int(2024))]),
                row(&[("id", Value::Int(2)), ("year", Value::Int(2023))]),
                row(&[("id", Value::Int(3)), ("year", Value::Int(2024))]),
            ],
        );
        let rows = handler.scan(
            &books_table(),
            None,
            &[FDWPredicate {
                column: "year".into(),
                operator: PredicateOp::Eq,
                value: Value::Int(2024),
            }],
            None,
        );
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|r| r.get("year") == Some(&Value::Int(2024))));
    }

    #[test]
    fn projection_keeps_only_requested_columns() {
        let mut handler = MemoryHandler::new();
        handler.load(
            "books",
            vec![row(&[
                ("id", Value::Int(1)),
                ("title", Value::Str("a".into())),
                ("year", Value::Int(2024)),
            ])],
        );
        let cols = ["title".to_string()];
        let rows = handler.scan(&books_table(), Some(&cols), &[], None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert!(rows[0].contains_key("title"));
        assert!(!rows[0].contains_key("id"));
    }

    #[test]
    fn limit_caps_result_count() {
        let mut handler = MemoryHandler::new();
        let rs: Vec<Row> = (0..10).map(|i| row(&[("id", Value::Int(i))])).collect();
        handler.load("books", rs);
        let rows = handler.scan(&books_table(), None, &[], Some(3));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn like_pattern_matches_with_percent_and_underscore() {
        assert!(sql_like("alpha", "a%a"));
        assert!(sql_like("alpha", "alp_a"));
        assert!(sql_like("alpha", "%pha"));
        assert!(!sql_like("alpha", "beta"));
    }

    #[test]
    fn in_predicate_filters_by_membership() {
        let mut handler = MemoryHandler::new();
        handler.load(
            "books",
            vec![
                row(&[("id", Value::Int(1))]),
                row(&[("id", Value::Int(2))]),
                row(&[("id", Value::Int(3))]),
            ],
        );
        let rows = handler.scan(
            &books_table(),
            None,
            &[FDWPredicate {
                column: "id".into(),
                operator: PredicateOp::In,
                value: Value::List(vec![Value::Int(1), Value::Int(3)]),
            }],
            None,
        );
        assert_eq!(rows.len(), 2);
    }
}
