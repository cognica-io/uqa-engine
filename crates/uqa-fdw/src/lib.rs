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

// The DuckDB and Arrow handlers wrap native C/C++ engines that do not
// exist on the browser (emscripten) target; the registry types and the
// in-memory handler below stay available everywhere.
#[cfg(not(target_os = "emscripten"))]
pub mod arrow_flight;
#[cfg(not(target_os = "emscripten"))]
pub mod arrow_ipc;
#[cfg(not(target_os = "emscripten"))]
pub mod duckdb;

#[cfg(not(target_os = "emscripten"))]
pub use arrow_flight::{
    build_where_clause as build_arrow_flight_where_clause,
    prepare_query as arrow_flight_prepare_query, quote_literal as arrow_flight_quote_literal,
    ArrowFlightPrepareError,
};
#[cfg(not(target_os = "emscripten"))]
pub use arrow_ipc::ArrowIpcHandler as ArrowHandler;
#[cfg(not(target_os = "emscripten"))]
pub use arrow_ipc::{ArrowIpcHandler, ArrowIpcPrepareError};
#[cfg(not(target_os = "emscripten"))]
pub use duckdb::{
    build_where_clause as build_duckdb_where_clause, normalize_source as duckdb_normalize_source,
    prepare_query as duckdb_prepare_query, DuckDBHandler, DuckDBPrepareError, FILE_READERS,
};

use std::collections::BTreeMap;

use uqa_core::Value;

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSubtype {
    Integer,
    BigInteger,
    Numeric,
    Date,
    Timestamp,
    TimestampTz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnType {
    SmallInteger,
    Integer,
    BigInteger,
    Oid,
    Xid,
    Real,
    DoublePrecision,
    Numeric {
        precision: Option<u32>,
        scale: Option<i32>,
    },
    Text,
    RefCursor,
    Name,
    Uuid,
    Varchar(Option<u32>),
    Bpchar,
    Character(u32),
    Bool,
    Bytes,
    InternalChar,
    Regproc,
    Regclass,
    Regnamespace,
    Regtype,
    PgNodeTree,
    AclItem,
    Int2Vector,
    OidVector,
    AnyArray,
    Record,
    Json,
    JsonB,
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
    Range(RangeSubtype),
    Multirange(RangeSubtype),
    Vector(u32),
    Tensor(u32),
    Domain {
        schema: String,
        name: String,
        oid: u32,
        base: Box<ColumnType>,
    },
    /// An array whose element metadata is preserved across the SQL/FDW boundary.
    Array(Box<ColumnType>),
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
    ILike,
    NotILike,
}

impl PredicateOp {
    /// Render the operator as the SQL token used in a `WHERE` clause.
    /// `In` is intentionally absent here -- callers wrap the IN list
    /// with the column themselves.
    pub fn sql_token(self) -> &'static str {
        match self {
            PredicateOp::Eq => "=",
            PredicateOp::NotEq => "!=",
            PredicateOp::Lt => "<",
            PredicateOp::LtEq => "<=",
            PredicateOp::Gt => ">",
            PredicateOp::GtEq => ">=",
            PredicateOp::In => "IN",
            PredicateOp::Like => "LIKE",
            PredicateOp::NotLike => "NOT LIKE",
            PredicateOp::ILike => "ILIKE",
            PredicateOp::NotILike => "NOT ILIKE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FDWPredicate {
    pub column: String,
    pub operator: PredicateOp,
    /// Scalar literal for comparisons; a `Value::List` for `In`.
    pub value: Value,
}

pub type Row = BTreeMap<String, Value>;
pub type RowStream = Box<dyn Iterator<Item = Result<Row, FDWError>> + Send>;

#[derive(Debug, thiserror::Error)]
pub enum FDWError {
    #[cfg(not(target_os = "emscripten"))]
    #[error(transparent)]
    DuckDBPrepare(#[from] DuckDBPrepareError),
    #[cfg(not(target_os = "emscripten"))]
    #[error(transparent)]
    ArrowIpcPrepare(#[from] ArrowIpcPrepareError),
    #[cfg(not(target_os = "emscripten"))]
    #[error(transparent)]
    DuckDB(#[from] ::duckdb::Error),
    #[cfg(not(target_os = "emscripten"))]
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Unsupported FDW value: {0}")]
    UnsupportedValue(String),
    #[error("Foreign table `{0}` is not loaded by the memory FDW")]
    UnknownTable(String),
    #[error("Invalid FDW predicate: {0}")]
    InvalidPredicate(String),
    #[error("{0}")]
    Other(String),
}

pub trait FDWHandler: Send + Sync {
    fn scan(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<u64>,
    ) -> Result<Vec<Row>, FDWError>;

    /// Pull-based scan ABI. Existing wrappers inherit the vector adapter;
    /// wrappers capable of cursor/batch reads can override this method and
    /// avoid a cardinality-sized intermediate allocation.
    fn scan_stream(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<u64>,
    ) -> Result<RowStream, FDWError> {
        Ok(Box::new(
            self.scan(table, columns, predicates, limit)?
                .into_iter()
                .map(Ok),
        ))
    }

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
        limit: Option<u64>,
    ) -> Result<Vec<Row>, FDWError> {
        let Some(rows) = self.tables.get(&table.name) else {
            return Err(FDWError::UnknownTable(table.name.clone()));
        };
        let mut out: Vec<Row> = Vec::new();
        for row in rows {
            if limit.is_some_and(|cap| limit_reached(out.len(), cap)) {
                break;
            }
            if !row_matches_predicates(row, predicates)? {
                continue;
            }
            out.push(project_row(row, columns));
        }
        Ok(out)
    }
}

pub fn row_matches_predicates(row: &Row, predicates: &[FDWPredicate]) -> Result<bool, FDWError> {
    for predicate in predicates {
        if !eval_predicate(predicate, row)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn project_row(row: &Row, columns: Option<&[String]>) -> Row {
    match columns {
        Some(cols) => {
            let mut keep = Row::new();
            for col in cols {
                keep.insert(col.clone(), row.get(col).cloned().unwrap_or(Value::Null));
            }
            keep
        }
        None => row.clone(),
    }
}

fn eval_predicate(p: &FDWPredicate, row: &Row) -> Result<bool, FDWError> {
    let lhs = row.get(&p.column);
    let matches = match p.operator {
        PredicateOp::Eq if matches!(p.value, Value::Null) => is_null(lhs),
        PredicateOp::Eq => {
            lhs.is_some_and(|value| !matches!(value, Value::Null) && value == &p.value)
        }
        PredicateOp::NotEq if matches!(p.value, Value::Null) => is_non_null(lhs),
        PredicateOp::NotEq => {
            lhs.is_some_and(|value| !matches!(value, Value::Null) && value != &p.value)
        }
        PredicateOp::Lt => compare_non_null(lhs, &p.value, p, |lhs, rhs| lhs < rhs)?,
        PredicateOp::LtEq => compare_non_null(lhs, &p.value, p, |lhs, rhs| lhs <= rhs)?,
        PredicateOp::Gt => compare_non_null(lhs, &p.value, p, |lhs, rhs| lhs > rhs)?,
        PredicateOp::GtEq => compare_non_null(lhs, &p.value, p, |lhs, rhs| lhs >= rhs)?,
        PredicateOp::In => match (&p.value, lhs) {
            (Value::List(items), Some(value)) if !matches!(value, Value::Null) => items
                .iter()
                .any(|item| !matches!(item, Value::Null) && item == value),
            (Value::List(_), None | Some(Value::Null)) => false,
            (other, _) => {
                return Err(FDWError::InvalidPredicate(format!(
                    "IN on `{}` requires a list, got {other:?}",
                    p.column
                )));
            }
        },
        PredicateOp::Like => string_predicate(lhs, &p.value, p, false, false)?,
        PredicateOp::NotLike => string_predicate(lhs, &p.value, p, false, true)?,
        PredicateOp::ILike => string_predicate(lhs, &p.value, p, true, false)?,
        PredicateOp::NotILike => string_predicate(lhs, &p.value, p, true, true)?,
    };
    Ok(matches)
}

fn is_null(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(_) => false,
    }
}

fn is_non_null(value: Option<&Value>) -> bool {
    value.is_some_and(|value| !matches!(value, Value::Null))
}

fn compare_non_null(
    lhs: Option<&Value>,
    rhs: &Value,
    predicate: &FDWPredicate,
    compare: impl FnOnce(&Value, &Value) -> bool,
) -> Result<bool, FDWError> {
    if matches!(rhs, Value::Null) {
        return Err(FDWError::InvalidPredicate(format!(
            "{:?} cannot compare `{}` with NULL",
            predicate.operator, predicate.column
        )));
    }
    Ok(lhs.is_some_and(|lhs| !matches!(lhs, Value::Null) && compare(lhs, rhs)))
}

pub(crate) fn limit_reached(len: usize, cap: u64) -> bool {
    usize::try_from(cap).is_ok_and(|cap| len >= cap)
}

fn string_predicate(
    lhs: Option<&Value>,
    rhs: &Value,
    predicate: &FDWPredicate,
    case_insensitive: bool,
    negate: bool,
) -> Result<bool, FDWError> {
    let Value::Str(pattern) = rhs else {
        return Err(FDWError::InvalidPredicate(format!(
            "{:?} on `{}` requires a string pattern",
            predicate.operator, predicate.column
        )));
    };
    let Some(lhs) = lhs else {
        return Ok(false);
    };
    let Value::Str(haystack) = lhs else {
        if matches!(lhs, Value::Null) {
            return Ok(false);
        }
        return Err(FDWError::InvalidPredicate(format!(
            "{:?} on `{}` requires string input",
            predicate.operator, predicate.column
        )));
    };
    let matched = if case_insensitive {
        sql_like(&haystack.to_lowercase(), &pattern.to_lowercase())?
    } else {
        sql_like(haystack, pattern)?
    };
    if negate {
        Ok(!matched)
    } else {
        Ok(matched)
    }
}

/// SQL `LIKE` matcher with `%` (zero+ chars) and `_` (single char)
/// wildcards. Greedy backtracking; safe on non-ASCII because we work
/// in `char`s, not bytes.
fn sql_like(haystack: &str, pattern: &str) -> Result<bool, FDWError> {
    let h: Vec<char> = haystack.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_match(&h, &p)
}

fn like_match(h: &[char], p: &[char]) -> Result<bool, FDWError> {
    let mut hi = 0;
    let mut pi = 0;
    let mut star_h: Option<usize> = None;
    let mut star_p: Option<usize> = None;
    while hi < h.len() {
        match p.get(pi) {
            Some('\\') => {
                let Some(literal) = p.get(pi + 1) else {
                    return Err(FDWError::InvalidPredicate(
                        "LIKE pattern must not end with escape character".into(),
                    ));
                };
                if *literal == h[hi] {
                    hi += 1;
                    pi += 2;
                } else if let (Some(sp), Some(sh)) = (star_p, star_h) {
                    pi = sp + 1;
                    star_h = Some(sh + 1);
                    hi = sh + 1;
                } else {
                    return Ok(false);
                }
            }
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
                    return Ok(false);
                }
            }
        }
    }
    while p.get(pi) == Some(&'%') {
        pi += 1;
    }
    Ok(pi == p.len())
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
        let rows = handler.scan(&books_table(), None, &[], None).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn memory_handler_does_not_treat_an_unloaded_table_as_empty() {
        let handler = MemoryHandler::new();
        let error = handler.scan(&books_table(), None, &[], None).unwrap_err();
        assert!(matches!(error, FDWError::UnknownTable(name) if name == "books"));
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
        let rows = handler
            .scan(
                &books_table(),
                None,
                &[FDWPredicate {
                    column: "year".into(),
                    operator: PredicateOp::Eq,
                    value: Value::Int(2024),
                }],
                None,
            )
            .unwrap();
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
        let rows = handler
            .scan(&books_table(), Some(&cols), &[], None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert!(rows[0].contains_key("title"));
        assert!(!rows[0].contains_key("id"));
    }

    #[test]
    fn projection_represents_missing_columns_as_null() {
        let row = row(&[("id", Value::Int(1))]);
        let projected = project_row(&row, Some(&["missing".to_string()]));
        assert_eq!(projected.get("missing"), Some(&Value::Null));
    }

    #[test]
    fn malformed_in_predicate_is_an_error_and_not_an_empty_match() {
        let row = row(&[("id", Value::Int(1))]);
        let predicate = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::In,
            value: Value::Int(1),
        };
        assert!(row_matches_predicates(&row, &[predicate]).is_err());
    }

    #[test]
    fn not_like_does_not_match_missing_values_and_rejects_non_text_values() {
        let predicate = FDWPredicate {
            column: "title".into(),
            operator: PredicateOp::NotLike,
            value: Value::Str("x%".into()),
        };
        assert!(!row_matches_predicates(&Row::new(), std::slice::from_ref(&predicate)).unwrap());
        assert!(row_matches_predicates(&row(&[("title", Value::Int(7))]), &[predicate]).is_err());
    }

    #[test]
    fn null_predicates_match_sql_filter_semantics() {
        let is_null = FDWPredicate {
            column: "missing".into(),
            operator: PredicateOp::Eq,
            value: Value::Null,
        };
        assert!(row_matches_predicates(&Row::new(), std::slice::from_ref(&is_null)).unwrap());
        assert!(row_matches_predicates(&row(&[("missing", Value::Null)]), &[is_null]).unwrap());

        let not_equal = FDWPredicate {
            column: "value".into(),
            operator: PredicateOp::NotEq,
            value: Value::Int(1),
        };
        assert!(!row_matches_predicates(&row(&[("value", Value::Null)]), &[not_equal]).unwrap());

        let invalid_order = FDWPredicate {
            column: "value".into(),
            operator: PredicateOp::Lt,
            value: Value::Null,
        };
        assert!(row_matches_predicates(&Row::new(), &[invalid_order]).is_err());
    }

    #[test]
    fn limit_caps_result_count() {
        let mut handler = MemoryHandler::new();
        let rs: Vec<Row> = (0..10).map(|i| row(&[("id", Value::Int(i))])).collect();
        handler.load("books", rs);
        let rows = handler.scan(&books_table(), None, &[], Some(3)).unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn like_pattern_matches_with_percent_and_underscore() {
        assert!(sql_like("alpha", "a%a").unwrap());
        assert!(sql_like("alpha", "alp_a").unwrap());
        assert!(sql_like("alpha", "%pha").unwrap());
        assert!(!sql_like("alpha", "beta").unwrap());
        assert!(sql_like("a_b", r"a\_b").unwrap());
        assert!(!sql_like("a", r"a\").unwrap());
        assert!(sql_like("ab", r"a\").is_err());
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
        let rows = handler
            .scan(
                &books_table(),
                None,
                &[FDWPredicate {
                    column: "id".into(),
                    operator: PredicateOp::In,
                    value: Value::List(vec![Value::Int(1), Value::Int(3)]),
                }],
                None,
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
    }
}
