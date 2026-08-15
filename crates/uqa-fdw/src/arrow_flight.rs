//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ArrowFlightSQLFDWHandler`: remote Arrow Flight SQL handler.
//!
//! Arrow Flight SQL generation: SQL literal quoting ([`quote_literal`]), `WHERE`
//! clause assembly ([`build_where_clause`]), and the full SELECT /
//! pre-existing-query builder ([`prepare_query`]). Actual Flight
//! SQL execution is left to the caller because the `arrow-flight`
//! crate carries a heavy dependency tree; integrators wire up the
//! optional crate at the boundary and run the prepared query string
//! themselves.
//!
//! Server options:
//!
//! * `host`     -- hostname of the Flight SQL server.
//! * `port`     -- port number (default `8815`).
//! * `tls`      -- `"true"` to enable TLS.
//! * `username` / `password` -- optional basic-token auth.
//!
//! Foreign table options:
//!
//! * `source` -- table name on the remote server.
//! * `query`  -- pre-built SQL query (takes precedence over `source`).

use std::fmt::Write as _;

use uqa_core::Value;

use crate::{FDWPredicate, ForeignTable, PredicateOp};

/// Format a `Value` as an Arrow-Flight-SQL literal.
///
/// Strings are single-quoted with embedded single quotes doubled.
/// Booleans render as `TRUE` / `FALSE`. Numerics render as their
/// `Display` form. `Value::Null` returns `NULL` -- callers usually
/// route through the IS NULL / IS NOT NULL branch instead, but
/// emitting `NULL` keeps the single-value codepath total.
pub fn quote_literal(value: &Value) -> Result<String, ArrowFlightPrepareError> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_finite() => format!("{f}"),
        Value::Float(f) => {
            return Err(ArrowFlightPrepareError::UnsupportedLiteral(format!(
                "non-finite float {f}"
            )));
        }
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) | Value::FixedChar(s) => {
            let escaped = s.replace('\'', "''");
            format!("'{escaped}'")
        }
        Value::Json(text) | Value::JsonB(text) => {
            let escaped = text.replace('\'', "''");
            let type_name = if matches!(value, Value::Json(_)) {
                "JSON"
            } else {
                "JSONB"
            };
            format!("CAST('{escaped}' AS {type_name})")
        }
        Value::Bytes(b) => {
            let capacity = b.len().checked_mul(2).ok_or_else(|| {
                ArrowFlightPrepareError::UnsupportedLiteral(
                    "binary literal length overflows usize".into(),
                )
            })?;
            let mut hex = String::new();
            hex.try_reserve_exact(capacity).map_err(|error| {
                ArrowFlightPrepareError::UnsupportedLiteral(format!(
                    "failed to allocate binary literal: {error}"
                ))
            })?;
            for byte in b {
                write!(hex, "{byte:02X}").map_err(|error| {
                    ArrowFlightPrepareError::UnsupportedLiteral(format!(
                        "failed to encode binary literal: {error}"
                    ))
                })?;
            }
            format!("X'{hex}'")
        }
        Value::Temporal(t) => {
            let escaped = t.to_sql_string().replace('\'', "''");
            format!("'{escaped}'")
        }
        Value::Array(array) => {
            if array.lower_bounds().iter().any(|lower| *lower != 1) {
                return Err(ArrowFlightPrepareError::UnsupportedLiteral(
                    "Flight SQL cannot portably represent non-one-based array literals".into(),
                ));
            }
            format!(
                "ARRAY[{}]",
                array
                    .elements()
                    .iter()
                    .map(quote_literal)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )
        }
        Value::List(items) => {
            // Used inside an IN list; emit a comma-separated literal
            // tuple. Outer caller decides whether to wrap in `(...)`.
            items
                .iter()
                .map(quote_literal)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        }
        Value::Row(_) | Value::Record(_) | Value::Map(_) => {
            return Err(ArrowFlightPrepareError::UnsupportedLiteral(
                "composite and map values have no portable Flight SQL literal".into(),
            ));
        }
    })
}

/// Render a Flight-SQL `WHERE` fragment from pushdown predicates. Flight SQL
/// has no parameter-binding API, so literal values are inlined
/// via [`quote_literal`].
pub fn build_where_clause(predicates: &[FDWPredicate]) -> Result<String, ArrowFlightPrepareError> {
    let mut clauses: Vec<String> = Vec::with_capacity(predicates.len());
    for p in predicates {
        let column = quote_identifier(&p.column);
        match (&p.value, p.operator) {
            (Value::Null, PredicateOp::Eq) => {
                clauses.push(format!("{column} IS NULL"));
            }
            (Value::Null, PredicateOp::NotEq) => {
                clauses.push(format!("{column} IS NOT NULL"));
            }
            (Value::Null, operator) => {
                return Err(ArrowFlightPrepareError::InvalidPredicate(format!(
                    "{operator:?} cannot compare `{}` with NULL",
                    p.column
                )));
            }
            (Value::List(items), PredicateOp::In) => {
                if items.is_empty() {
                    clauses.push("FALSE".to_string());
                    continue;
                }
                let inner = items
                    .iter()
                    .map(quote_literal)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                clauses.push(format!("{column} IN ({inner})"));
            }
            (_, PredicateOp::In) => {
                return Err(ArrowFlightPrepareError::InvalidPredicate(format!(
                    "IN on `{}` requires a list",
                    p.column
                )));
            }
            (_, op) => {
                clauses.push(format!(
                    "{column} {} {}",
                    op.sql_token(),
                    quote_literal(&p.value)?
                ));
            }
        }
    }
    Ok(clauses.join(" AND "))
}

/// Build the SQL query the handler ships to the remote Flight SQL
/// server. The `query` option, when present, takes precedence over
/// `source`. A prebuilt query is wrapped as a subquery so projection,
/// predicates, and limits are always applied at this boundary.
pub fn prepare_query(
    table: &ForeignTable,
    columns: Option<&[String]>,
    predicates: &[FDWPredicate],
    limit: Option<u64>,
) -> Result<String, ArrowFlightPrepareError> {
    let (source, default_to_star) = if let Some(query) = table.options.get("query") {
        let query = query.trim().trim_end_matches(';').trim();
        if query.is_empty() {
            return Err(ArrowFlightPrepareError::EmptyQuery(table.name.clone()));
        }
        (format!("({query}) AS uqa_fdw_source"), true)
    } else {
        let Some(source) = table.options.get("source") else {
            return Err(ArrowFlightPrepareError::MissingSourceOrQuery(
                table.name.clone(),
            ));
        };
        (quote_identifier(source), false)
    };

    let column_names = match columns {
        Some(columns) if !columns.is_empty() => columns.to_vec(),
        _ if default_to_star => Vec::new(),
        _ => table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    };
    let columns = if column_names.is_empty() {
        "*".to_string()
    } else {
        column_names
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut query = format!("SELECT {columns} FROM {source}");

    if !predicates.is_empty() {
        let where_sql = build_where_clause(predicates)?;
        if !where_sql.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&where_sql);
        }
    }

    if let Some(n) = limit {
        use std::fmt::Write as _;
        write!(query, " LIMIT {n}").map_err(|error| {
            ArrowFlightPrepareError::UnsupportedLiteral(format!("failed to append LIMIT: {error}"))
        })?;
    }

    Ok(query)
}

fn quote_identifier(value: &str) -> String {
    value
        .split('.')
        .map(|part| {
            if part == "*" {
                "*".to_string()
            } else {
                format!("\"{}\"", part.replace('"', "\"\""))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[derive(Debug, thiserror::Error)]
pub enum ArrowFlightPrepareError {
    #[error("Foreign table `{0}` missing required option `source` or `query`")]
    MissingSourceOrQuery(String),
    #[error("Foreign table `{0}` has an empty `query` option")]
    EmptyQuery(String),
    #[error("Invalid Arrow Flight pushdown predicate: {0}")]
    InvalidPredicate(String),
    #[error("Unsupported Arrow Flight SQL literal: {0}")]
    UnsupportedLiteral(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnDef, ColumnType};
    use std::collections::BTreeMap;

    fn table_with_options<const N: usize>(opts: [(&str, &str); N]) -> ForeignTable {
        ForeignTable {
            name: "remote".into(),
            server_name: "flight".into(),
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    ty: ColumnType::Integer,
                },
                ColumnDef {
                    name: "name".into(),
                    ty: ColumnType::Text,
                },
            ],
            options: opts
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn quote_literal_escapes_single_quotes() {
        assert_eq!(
            quote_literal(&Value::Str("it's".into())).unwrap(),
            "'it''s'"
        );
    }

    #[test]
    fn quote_literal_renders_int_and_bool() {
        assert_eq!(quote_literal(&Value::Int(42)).unwrap(), "42");
        assert_eq!(quote_literal(&Value::Bool(true)).unwrap(), "TRUE");
    }

    #[test]
    fn build_where_clause_inlines_literals() {
        let preds = vec![
            FDWPredicate {
                column: "year".into(),
                operator: PredicateOp::Eq,
                value: Value::Int(2024),
            },
            FDWPredicate {
                column: "name".into(),
                operator: PredicateOp::Like,
                value: Value::Str("alpha%".into()),
            },
        ];
        let sql = build_where_clause(&preds).unwrap();
        assert_eq!(sql, "\"year\" = 2024 AND \"name\" LIKE 'alpha%'");
    }

    #[test]
    fn build_where_clause_emits_in_list() {
        let preds = vec![FDWPredicate {
            column: "country".into(),
            operator: PredicateOp::In,
            value: Value::List(vec![Value::Str("US".into()), Value::Str("KR".into())]),
        }];
        let sql = build_where_clause(&preds).unwrap();
        assert_eq!(sql, "\"country\" IN ('US', 'KR')");
    }

    #[test]
    fn prepare_query_assembles_select_when_no_query_option() {
        let table = table_with_options([("source", "books")]);
        let q = prepare_query(&table, None, &[], None).unwrap();
        assert_eq!(q, "SELECT \"id\", \"name\" FROM \"books\"");
    }

    #[test]
    fn prepare_query_wraps_query_option_and_preserves_pushdown() {
        let table = table_with_options([("query", "SELECT id FROM books WHERE year = 2024")]);
        let preds = vec![FDWPredicate {
            column: "year".into(),
            operator: PredicateOp::Eq,
            value: Value::Int(2025),
        }];
        let q = prepare_query(&table, None, &preds, None).unwrap();
        assert_eq!(
            q,
            "SELECT * FROM (SELECT id FROM books WHERE year = 2024) AS uqa_fdw_source WHERE \"year\" = 2025"
        );
    }

    #[test]
    fn prepare_query_appends_limit_when_absent() {
        let table = table_with_options([("source", "books")]);
        let q = prepare_query(&table, None, &[], Some(50)).unwrap();
        assert!(q.ends_with(" LIMIT 50"));
    }

    #[test]
    fn prepare_query_errors_when_source_and_query_missing() {
        let table = table_with_options([]);
        let err = prepare_query(&table, None, &[], None).unwrap_err();
        assert!(matches!(
            err,
            ArrowFlightPrepareError::MissingSourceOrQuery(name) if name == "remote"
        ));
    }

    #[test]
    fn unsupported_literals_and_malformed_in_predicates_fail() {
        assert!(quote_literal(&Value::Map(BTreeMap::new())).is_err());
        assert!(quote_literal(&Value::Float(f64::NAN)).is_err());
        let predicate = FDWPredicate {
            column: "id".into(),
            operator: PredicateOp::In,
            value: Value::Int(1),
        };
        assert!(build_where_clause(&[predicate]).is_err());
    }
}
