//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ArrowFlightSQLFDWHandler`: remote Arrow Flight SQL handler.
//!
//! 1:1 port of `uqa.fdw.arrow_handler`. The Rust port covers SQL
//! generation: SQL literal quoting ([`quote_literal`]), `WHERE`
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

use uqa_core::Value;

use crate::{FDWPredicate, ForeignTable, PredicateOp};

/// Format a `Value` as an Arrow-Flight-SQL literal.
///
/// Strings are single-quoted with embedded single quotes doubled.
/// Booleans render as `TRUE` / `FALSE`. Numerics render as their
/// `Display` form. `Value::Null` returns `NULL` -- callers usually
/// route through the IS NULL / IS NOT NULL branch instead, but
/// emitting `NULL` keeps the single-value codepath total.
pub fn quote_literal(value: &Value) -> String {
    match value {
        Value::Null | Value::Map(_) => "NULL".into(),
        Value::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Str(s) => {
            let escaped = s.replace('\'', "''");
            format!("'{escaped}'")
        }
        Value::Bytes(b) => {
            let escaped = format!("<{} bytes>", b.len()).replace('\'', "''");
            format!("'{escaped}'")
        }
        Value::List(items) => {
            // Used inside an IN list; emit a comma-separated literal
            // tuple. Outer caller decides whether to wrap in `(...)`.
            items
                .iter()
                .map(quote_literal)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// Render a Flight-SQL `WHERE` fragment from pushdown predicates.
/// Mirrors `_build_where_clause` in the Python reference. Flight
/// SQL has no parameter binding API, so literal values are inlined
/// via [`quote_literal`].
pub fn build_where_clause(predicates: &[FDWPredicate]) -> String {
    let mut clauses: Vec<String> = Vec::with_capacity(predicates.len());
    for p in predicates {
        match (&p.value, p.operator) {
            (Value::Null, PredicateOp::Eq) => {
                clauses.push(format!("{} IS NULL", p.column));
            }
            (Value::Null, _) => {
                clauses.push(format!("{} IS NOT NULL", p.column));
            }
            (Value::List(items), PredicateOp::In) => {
                let inner = items
                    .iter()
                    .map(quote_literal)
                    .collect::<Vec<_>>()
                    .join(", ");
                clauses.push(format!("{} IN ({inner})", p.column));
            }
            (_, op) => {
                clauses.push(format!(
                    "{} {} {}",
                    p.column,
                    op.sql_token(),
                    quote_literal(&p.value)
                ));
            }
        }
    }
    clauses.join(" AND ")
}

/// Build the SQL query the handler ships to the remote Flight SQL
/// server. The `query` option, when present, takes precedence over
/// `source` -- matching the Python reference. Predicates and limit
/// are appended only when the prepared query doesn't already
/// contain `WHERE` / `LIMIT` (case-insensitive).
pub fn prepare_query(
    table: &ForeignTable,
    columns: Option<&[String]>,
    predicates: &[FDWPredicate],
    limit: Option<u64>,
) -> Result<String, ArrowFlightPrepareError> {
    let mut query = if let Some(q) = table.options.get("query") {
        q.clone()
    } else {
        let Some(source) = table.options.get("source") else {
            return Err(ArrowFlightPrepareError::MissingSourceOrQuery(
                table.name.clone(),
            ));
        };
        let cols = match columns {
            Some(cs) if !cs.is_empty() => cs.join(", "),
            _ => table
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        };
        format!("SELECT {cols} FROM {source}")
    };

    if !predicates.is_empty() && !contains_keyword(&query, "WHERE") {
        let where_sql = build_where_clause(predicates);
        if !where_sql.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&where_sql);
        }
    }

    if let Some(n) = limit {
        if !contains_keyword(&query, "LIMIT") {
            use std::fmt::Write as _;
            let _ = write!(query, " LIMIT {n}");
        }
    }

    Ok(query)
}

fn contains_keyword(haystack: &str, keyword: &str) -> bool {
    let upper = haystack.to_ascii_uppercase();
    let key = keyword.to_ascii_uppercase();
    upper.contains(&format!(" {key} ")) || upper.ends_with(&format!(" {key}"))
}

#[derive(Debug, thiserror::Error)]
pub enum ArrowFlightPrepareError {
    #[error("Foreign table `{0}` missing required option `source` or `query`")]
    MissingSourceOrQuery(String),
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
        assert_eq!(quote_literal(&Value::Str("it's".into())), "'it''s'");
    }

    #[test]
    fn quote_literal_renders_int_and_bool() {
        assert_eq!(quote_literal(&Value::Int(42)), "42");
        assert_eq!(quote_literal(&Value::Bool(true)), "TRUE");
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
        let sql = build_where_clause(&preds);
        assert_eq!(sql, "year = 2024 AND name LIKE 'alpha%'");
    }

    #[test]
    fn build_where_clause_emits_in_list() {
        let preds = vec![FDWPredicate {
            column: "country".into(),
            operator: PredicateOp::In,
            value: Value::List(vec![Value::Str("US".into()), Value::Str("KR".into())]),
        }];
        let sql = build_where_clause(&preds);
        assert_eq!(sql, "country IN ('US', 'KR')");
    }

    #[test]
    fn prepare_query_assembles_select_when_no_query_option() {
        let table = table_with_options([("source", "books")]);
        let q = prepare_query(&table, None, &[], None).unwrap();
        assert_eq!(q, "SELECT id, name FROM books");
    }

    #[test]
    fn prepare_query_uses_query_option_verbatim_when_present() {
        let table = table_with_options([("query", "SELECT id FROM books WHERE year = 2024")]);
        let preds = vec![FDWPredicate {
            column: "year".into(),
            operator: PredicateOp::Eq,
            value: Value::Int(2025),
        }];
        // The pre-built query already has WHERE -- predicates are
        // dropped, mirroring the Python reference.
        let q = prepare_query(&table, None, &preds, None).unwrap();
        assert_eq!(q, "SELECT id FROM books WHERE year = 2024");
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
        match err {
            ArrowFlightPrepareError::MissingSourceOrQuery(name) => {
                assert_eq!(name, "remote");
            }
        }
    }
}
