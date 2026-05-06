//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DuckDBFDWHandler`: in-process `DuckDB` foreign data wrapper.
//!
//! 1:1 port of `uqa.fdw.duckdb_handler`. The Rust port owns the SQL
//! generation half: source-string normalization
//! ([`normalize_source`]), parameterized `WHERE` clause assembly
//! ([`build_where_clause`]), and the full `SELECT` builder
//! ([`prepare_query`]). The actual `duckdb` execution is left to the
//! caller because pulling the `duckdb` C library into every Rust
//! workspace consumer is heavy; callers that need execution wire up
//! the optional `duckdb` crate at the integration boundary and run
//! the prepared `(sql, params)` tuple themselves.
//!
//! Server options (passed at the [`crate::ForeignServer`] level):
//!
//! * `database`   -- path to a `DuckDB` file (default `:memory:`).
//! * `extensions` -- comma-separated list of extensions to load.
//! * `s3_region`, `s3_access_key_id`, `s3_secret_access_key` --
//!   passed through to `SET s3_*`.
//!
//! Foreign table options (passed at [`crate::ForeignTable`]):
//!
//! * `source`            -- a `DuckDB` expression (e.g.
//!   `read_parquet('s3://...')`), an attached table name, or a bare
//!   file path that gets auto-wrapped in a `read_*()` call.
//! * `hive_partitioning` -- `"true"` to enable Hive-style partition
//!   discovery on auto-wrapped sources.

use uqa_core::Value;

use crate::{FDWPredicate, ForeignTable, PredicateOp};

/// File extensions `DuckDB` reads natively via `read_*` table
/// functions. Mirrors `_FILE_READERS` in the Python reference.
pub const FILE_READERS: &[(&str, &str)] = &[
    (".parquet", "read_parquet"),
    (".csv", "read_csv"),
    (".json", "read_json"),
    (".ndjson", "read_json"),
];

/// Wrap bare file paths in the appropriate `DuckDB` reader function.
///
/// * Sources that already contain `(` are returned unchanged
///   (already a function call).
/// * Sources ending with one of the known file extensions are
///   wrapped in `read_*('path')`. When `hive_partitioning` is true,
///   `, hive_partitioning = true` is appended.
/// * Everything else (bare table names, attached views) is returned
///   as-is.
pub fn normalize_source(source: &str, hive_partitioning: bool) -> String {
    if source.contains('(') {
        return source.to_string();
    }
    let lower = source.to_ascii_lowercase();
    for (ext, reader) in FILE_READERS {
        if lower.ends_with(ext) {
            return if hive_partitioning {
                format!("{reader}('{source}', hive_partitioning = true)")
            } else {
                format!("{reader}('{source}')")
            };
        }
    }
    source.to_string()
}

/// Convert pushdown predicates into a `DuckDB`-style `WHERE` clause
/// with `?` placeholders plus the parameter vector. Mirrors
/// `_build_where_clause` in the Python reference.
///
/// Parameterized binding shields against SQL injection; the caller
/// ships `(sql, params)` to `duckdb::execute`.
pub fn build_where_clause(predicates: &[FDWPredicate]) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::with_capacity(predicates.len());
    let mut params: Vec<Value> = Vec::new();
    for p in predicates {
        match (&p.value, p.operator) {
            (Value::Null, PredicateOp::Eq) => {
                clauses.push(format!("{} IS NULL", p.column));
            }
            (Value::Null, _) => {
                clauses.push(format!("{} IS NOT NULL", p.column));
            }
            (Value::List(items), PredicateOp::In) => {
                let placeholders = items.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                clauses.push(format!("{} IN ({placeholders})", p.column));
                params.extend(items.iter().cloned());
            }
            (_, op) => {
                clauses.push(format!("{} {} ?", p.column, op.sql_token()));
                params.push(p.value.clone());
            }
        }
    }
    (clauses.join(" AND "), params)
}

/// Render a full `DuckDB` `SELECT cols FROM source` query with the
/// optional `WHERE ...` and `LIMIT n` tails appended. Returns
/// `(sql, params)` -- the bind values are in declaration order
/// matching the `?` placeholders.
pub fn prepare_query(
    table: &ForeignTable,
    columns: Option<&[String]>,
    predicates: &[FDWPredicate],
    limit: Option<u64>,
) -> Result<(String, Vec<Value>), DuckDBPrepareError> {
    let Some(source) = table.options.get("source") else {
        return Err(DuckDBPrepareError::MissingSource(table.name.clone()));
    };
    let hive = table
        .options
        .get("hive_partitioning")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let normalized = normalize_source(source, hive);

    let cols = match columns {
        Some(cs) if !cs.is_empty() => cs.join(", "),
        _ => table
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
    };

    let mut sql = format!("SELECT {cols} FROM {normalized}");
    let (where_sql, params) = build_where_clause(predicates);
    if !where_sql.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_sql);
    }
    if let Some(n) = limit {
        use std::fmt::Write as _;
        let _ = write!(sql, " LIMIT {n}");
    }

    Ok((sql, params))
}

#[derive(Debug, thiserror::Error)]
pub enum DuckDBPrepareError {
    #[error("Foreign table `{0}` missing required option `source`")]
    MissingSource(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnDef, ColumnType};
    use std::collections::BTreeMap;

    fn books_table_with_source(source: &str) -> ForeignTable {
        let mut options = BTreeMap::new();
        options.insert("source".to_string(), source.to_string());
        ForeignTable {
            name: "books".into(),
            server_name: "duck".into(),
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    ty: ColumnType::Integer,
                },
                ColumnDef {
                    name: "title".into(),
                    ty: ColumnType::Text,
                },
            ],
            options,
        }
    }

    #[test]
    fn normalize_source_wraps_parquet_path() {
        let s = normalize_source("/data/books.parquet", false);
        assert_eq!(s, "read_parquet('/data/books.parquet')");
    }

    #[test]
    fn normalize_source_appends_hive_partitioning() {
        let s = normalize_source("/data/books.parquet", true);
        assert_eq!(
            s,
            "read_parquet('/data/books.parquet', hive_partitioning = true)"
        );
    }

    #[test]
    fn normalize_source_passes_function_calls_through() {
        let s = normalize_source("read_csv('s3://bucket/key.csv', delim = '|')", false);
        assert_eq!(s, "read_csv('s3://bucket/key.csv', delim = '|')");
    }

    #[test]
    fn normalize_source_passes_table_names_through() {
        let s = normalize_source("attached_db.books", false);
        assert_eq!(s, "attached_db.books");
    }

    #[test]
    fn build_where_clause_emits_placeholders_and_params() {
        let preds = vec![
            FDWPredicate {
                column: "year".into(),
                operator: PredicateOp::Eq,
                value: Value::Int(2024),
            },
            FDWPredicate {
                column: "country".into(),
                operator: PredicateOp::In,
                value: Value::List(vec![Value::Str("US".into()), Value::Str("KR".into())]),
            },
        ];
        let (sql, params) = build_where_clause(&preds);
        assert_eq!(sql, "year = ? AND country IN (?, ?)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn build_where_clause_handles_null_branch() {
        let preds = vec![FDWPredicate {
            column: "deleted_at".into(),
            operator: PredicateOp::Eq,
            value: Value::Null,
        }];
        let (sql, params) = build_where_clause(&preds);
        assert_eq!(sql, "deleted_at IS NULL");
        assert!(params.is_empty());
    }

    #[test]
    fn prepare_query_assembles_select_with_where_and_limit() {
        let table = books_table_with_source("/data/books.parquet");
        let preds = vec![FDWPredicate {
            column: "year".into(),
            operator: PredicateOp::Eq,
            value: Value::Int(2024),
        }];
        let (sql, params) = prepare_query(&table, None, &preds, Some(10)).unwrap();
        assert!(sql.contains("read_parquet('/data/books.parquet')"));
        assert!(sql.contains(" WHERE year = ?"));
        assert!(sql.ends_with(" LIMIT 10"));
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn prepare_query_errors_when_source_missing() {
        let table = ForeignTable {
            name: "books".into(),
            server_name: "duck".into(),
            columns: vec![],
            options: BTreeMap::new(),
        };
        let err = prepare_query(&table, None, &[], None).unwrap_err();
        match err {
            DuckDBPrepareError::MissingSource(name) => assert_eq!(name, "books"),
        }
    }

    #[test]
    fn prepare_query_projects_specific_columns() {
        let table = books_table_with_source("books");
        let cols = ["title".to_string()];
        let (sql, _) = prepare_query(&table, Some(&cols), &[], None).unwrap();
        assert!(sql.starts_with("SELECT title FROM"));
    }
}
