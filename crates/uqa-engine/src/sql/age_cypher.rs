//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE-compatible SQL table-function adapter for Cypher.
//!
//! The graph crate owns parsing and execution. This module only maps
//! PostgreSQL-style `FROM cypher(...) AS (...)` calls onto the engine's
//! registered graph workspaces and SQL row shape.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::Expr;
use uqa_sql::{ResultRow, SQLError};

use crate::Engine;

pub(super) fn build_rows(
    engine: &Engine,
    args: &[Expr],
    evaluated: &[Value],
    qualifier: Option<&str>,
    column_aliases: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
    if !(2..=3).contains(&evaluated.len()) {
        return Err(SQLError::TypeMismatch(
            "cypher requires 2-3 args (graph_name, query_string[, parameters])".into(),
        ));
    }
    if column_aliases.is_empty() {
        return Err(SQLError::TypeMismatch(
            "cypher requires a record definition: AS (column agtype, ...)".into(),
        ));
    }
    if args.len() == 3 && !is_valid_parameter_expr(&args[2]) {
        return Err(SQLError::TypeMismatch(
            "cypher parameters must be supplied through an SQL parameter".into(),
        ));
    }

    let graph = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => {
            return Err(SQLError::TypeMismatch(
                "cypher.graph_name must be string".into(),
            ))
        }
    };
    let query = match &evaluated[1] {
        Value::Str(s) => s.clone(),
        _ => {
            return Err(SQLError::TypeMismatch(
                "cypher.query_string must be string".into(),
            ))
        }
    };
    if !engine.has_graph(&graph) {
        return Err(SQLError::Unsupported(format!(
            "graph {graph:?} does not exist"
        )));
    }

    let params = match evaluated.get(2) {
        Some(value) => parameter_map(value)?,
        None => BTreeMap::new(),
    };
    let (cypher_columns, cypher_rows) = engine
        .run_cypher(&graph, &query, params)
        .map_err(|e| SQLError::Unsupported(format!("cypher: {e}")))?;
    if !cypher_columns.is_empty() && cypher_columns.len() != column_aliases.len() {
        return Err(SQLError::TypeMismatch(format!(
            "cypher returned {} columns but record definition has {}",
            cypher_columns.len(),
            column_aliases.len()
        )));
    }

    let mut out = Vec::with_capacity(cypher_rows.len());
    for src in cypher_rows {
        let mut row = ResultRow::new();
        for (idx, target_col) in column_aliases.iter().enumerate() {
            let value = cypher_columns
                .get(idx)
                .and_then(|source_col| src.get(source_col))
                .cloned()
                .unwrap_or(Value::Null);
            row.insert(target_col.clone(), value);
        }
        let row = match qualifier {
            Some(alias) => super::prefix_row(alias, &row),
            None => row,
        };
        out.push(row);
    }
    Ok(out)
}

fn is_valid_parameter_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Param(_) | Expr::Literal(Value::Null))
}

fn parameter_map(value: &Value) -> Result<BTreeMap<String, Value>, SQLError> {
    match value {
        Value::Null => Ok(BTreeMap::new()),
        Value::Map(map) => Ok(map.clone()),
        Value::Str(s) => {
            let parsed = serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid cypher parameters: {e}")))?;
            match super::json_to_core_value(parsed) {
                Value::Map(map) => Ok(map),
                _ => Err(SQLError::TypeMismatch(
                    "cypher parameters must be a map".into(),
                )),
            }
        }
        _ => Err(SQLError::TypeMismatch(
            "cypher parameters must be a map".into(),
        )),
    }
}
