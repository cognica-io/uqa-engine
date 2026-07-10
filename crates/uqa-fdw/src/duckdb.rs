//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DuckDBFDWHandler`: in-process `DuckDB` foreign data wrapper.
//!
//! Coverage for `uqa.fdw.duckdb_handler`. The UQA-RS implementation owns the SQL
//! generation and execution path: source-string normalization
//! ([`normalize_source`]), parameterized `WHERE` clause assembly
//! ([`build_where_clause`]), full `SELECT` builder ([`prepare_query`]),
//! and row materialization through the `duckdb` Rust crate.
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

use uqa_core::{TemporalValue, Value};

use crate::{FDWError, FDWHandler, FDWPredicate, ForeignServer, ForeignTable, PredicateOp, Row};

/// File extensions `DuckDB` reads natively via `read_*` table
/// functions. Mirrors `_FILE_READERS` in the canonical UQA behavior.
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
/// `_build_where_clause` in the canonical UQA behavior.
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

#[derive(Debug, Clone)]
pub struct DuckDBHandler {
    server: ForeignServer,
}

impl DuckDBHandler {
    pub fn new(server: ForeignServer) -> Self {
        Self { server }
    }

    fn open_connection(&self) -> Result<::duckdb::Connection, FDWError> {
        let database = self
            .server
            .options
            .get("database")
            .map_or(":memory:", String::as_str);
        let conn = if database == ":memory:" {
            ::duckdb::Connection::open_in_memory()?
        } else {
            ::duckdb::Connection::open(database)?
        };
        apply_server_options(&conn, &self.server)?;
        Ok(conn)
    }
}

impl FDWHandler for DuckDBHandler {
    fn scan(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<u64>,
    ) -> Result<Vec<Row>, FDWError> {
        let (sql, params) = prepare_query(table, columns, predicates, limit)?;
        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(&sql)?;
        let output_columns = output_columns(table, columns);
        let bind_values = params
            .iter()
            .map(uqa_value_to_duck_value)
            .collect::<Result<Vec<_>, _>>()?;
        let mapped = stmt.query_map(::duckdb::params_from_iter(bind_values.iter()), |row| {
            let mut out = Row::new();
            for (idx, name) in output_columns.iter().enumerate() {
                let value: ::duckdb::types::Value = row.get(idx)?;
                out.insert(name.clone(), duck_value_to_uqa(value));
            }
            Ok(out)
        })?;

        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        Ok(rows)
    }
}

fn output_columns(table: &ForeignTable, columns: Option<&[String]>) -> Vec<String> {
    match columns {
        Some(cols) if !cols.is_empty() => cols.to_vec(),
        _ => table.columns.iter().map(|c| c.name.clone()).collect(),
    }
}

fn apply_server_options(
    conn: &::duckdb::Connection,
    server: &ForeignServer,
) -> Result<(), FDWError> {
    if let Some(extensions) = server.options.get("extensions") {
        for ext in extensions
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(FDWError::Other(format!(
                    "Invalid DuckDB extension name `{ext}`"
                )));
            }
            conn.execute_batch(&format!("LOAD {ext}"))?;
        }
    }

    for key in [
        "s3_region",
        "s3_access_key_id",
        "s3_secret_access_key",
        "s3_endpoint",
        "s3_url_style",
    ] {
        if let Some(value) = server.options.get(key) {
            conn.execute_batch(&format!("SET {key} = {}", quote_literal(value)))?;
        }
    }
    Ok(())
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn uqa_value_to_duck_value(value: &Value) -> Result<::duckdb::types::Value, FDWError> {
    use ::duckdb::types::{TimeUnit, Value as DuckValue};
    Ok(match value {
        Value::Null => DuckValue::Null,
        Value::Bool(v) => DuckValue::Boolean(*v),
        Value::Int(v) => DuckValue::BigInt(*v),
        Value::Float(v) => DuckValue::Double(*v),
        Value::Decimal(v) => DuckValue::Text(v.to_sql_string()),
        Value::Str(v) => DuckValue::Text(v.clone()),
        Value::Bytes(v) => DuckValue::Blob(v.clone()),
        Value::Temporal(TemporalValue::Date { days }) => DuckValue::Date32(*days),
        Value::Temporal(TemporalValue::Time { micros }) => {
            DuckValue::Time64(TimeUnit::Microsecond, *micros)
        }
        Value::Temporal(
            TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros },
        ) => DuckValue::Timestamp(TimeUnit::Microsecond, *micros),
        Value::Temporal(TemporalValue::TimeTz { .. } | TemporalValue::Interval { .. }) => {
            DuckValue::Text(value_to_string(value))
        }
        Value::List(items) => DuckValue::List(
            items
                .iter()
                .map(uqa_value_to_duck_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Map(_) => {
            return Err(FDWError::UnsupportedValue(
                "map literals cannot be bound to DuckDB parameters".into(),
            ));
        }
    })
}

fn duck_value_to_uqa(value: ::duckdb::types::Value) -> Value {
    use ::duckdb::types::Value as DuckValue;
    match value {
        DuckValue::Null => Value::Null,
        DuckValue::Boolean(v) => Value::Bool(v),
        DuckValue::TinyInt(v) => Value::Int(i64::from(v)),
        DuckValue::SmallInt(v) => Value::Int(i64::from(v)),
        DuckValue::Int(v) => Value::Int(i64::from(v)),
        DuckValue::BigInt(v) => Value::Int(v),
        DuckValue::HugeInt(v) => i64::try_from(v).map_or(Value::Str(v.to_string()), Value::Int),
        DuckValue::UTinyInt(v) => Value::Int(i64::from(v)),
        DuckValue::USmallInt(v) => Value::Int(i64::from(v)),
        DuckValue::UInt(v) => Value::Int(i64::from(v)),
        DuckValue::UBigInt(v) => i64::try_from(v).map_or(Value::Str(v.to_string()), Value::Int),
        DuckValue::Float(v) => Value::Float(f64::from(v)),
        DuckValue::Double(v) => Value::Float(v),
        DuckValue::Decimal(v) => {
            let text = v.to_string();
            text.parse::<f64>()
                .map_or_else(|_| Value::Str(text), Value::Float)
        }
        DuckValue::Timestamp(unit, v) => Value::Temporal(TemporalValue::Timestamp {
            micros: unit.to_micros(v),
        }),
        DuckValue::Text(v) | DuckValue::Enum(v) => Value::Str(v),
        DuckValue::Blob(v) => Value::Bytes(v),
        DuckValue::Date32(days) => Value::Temporal(TemporalValue::Date { days }),
        DuckValue::Time64(unit, v) => Value::Temporal(TemporalValue::Time {
            micros: unit.to_micros(v),
        }),
        DuckValue::Interval {
            months,
            days,
            nanos,
        } => Value::Str(format!("{months} months {days} days {nanos} nanos")),
        DuckValue::List(items) | DuckValue::Array(items) => {
            Value::List(items.into_iter().map(duck_value_to_uqa).collect())
        }
        DuckValue::Struct(fields) => Value::Map(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), duck_value_to_uqa(v.clone())))
                .collect(),
        ),
        DuckValue::Map(fields) => Value::Map(
            fields
                .iter()
                .map(|(k, v)| {
                    (
                        duck_value_key_to_string(k.clone()),
                        duck_value_to_uqa(v.clone()),
                    )
                })
                .collect(),
        ),
        DuckValue::Union(v) => duck_value_to_uqa(*v),
    }
}

fn duck_value_key_to_string(value: ::duckdb::types::Value) -> String {
    match duck_value_to_uqa(value) {
        Value::Str(s) => s,
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Bool(b) => b.to_string(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Null => "null".into(),
        Value::Bytes(bytes) => format!("<{} bytes>", bytes.len()),
        Value::List(items) => format!("{items:?}"),
        Value::Map(map) => format!("{map:?}"),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Temporal(t) => t.to_sql_string(),
        Value::Decimal(d) => d.to_sql_string(),
        other => format!("{other:?}"),
    }
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

    #[test]
    fn duckdb_handler_scans_real_database_with_pushdown() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("books.duckdb");
        {
            let conn = ::duckdb::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE books (id INTEGER, title TEXT, year INTEGER);
                 INSERT INTO books VALUES
                   (1, 'Rust', 2024),
                   (2, 'Python', 2023),
                   (3, 'UQA', 2024);",
            )
            .unwrap();
        }

        let server = ForeignServer {
            name: "duck".into(),
            fdw_type: "duckdb_fdw".into(),
            options: [(
                "database".to_string(),
                db_path.to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect(),
        };
        let table = books_table_with_source("books");
        let handler = DuckDBHandler::new(server);
        let cols = ["title".to_string()];
        let rows = handler
            .scan(
                &table,
                Some(&cols),
                &[FDWPredicate {
                    column: "year".into(),
                    operator: PredicateOp::Eq,
                    value: Value::Int(2024),
                }],
                Some(1),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("title"), Some(&Value::Str("Rust".into())));
        assert!(!rows[0].contains_key("id"));
    }
}
