//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph invocation and physical row construction for SQL Cypher table functions.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_graph::cypher::{CypherError, ResultRow};
use uqa_sql::{semantics::age_cypher, SQLError, ScalarExpr};
use uqa_storage::StorageBackendResult;

use super::graph_effects::query_is_mutating;

/// Live graph access within the caller's existing statement and transaction boundary.
pub trait CypherTableRuntime {
    fn current_transaction_is_read_only(&self) -> bool;
    fn has_graph(&self, name: &str) -> StorageBackendResult<bool>;
    fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError>;
}

pub fn build_rows(
    runtime: &dyn CypherTableRuntime,
    args: &[ScalarExpr],
    evaluated: &[Value],
    column_aliases: &[String],
    column_types: &[String],
) -> Result<Vec<Vec<Value>>, SQLError> {
    let (graph, query) = age_cypher::analyze_call(args, evaluated, column_aliases)?;
    if runtime.current_transaction_is_read_only() && query_is_mutating(&query)? {
        return Err(SQLError::Routine {
            sqlstate: "25006".into(),
            message: "cannot execute SELECT in a read-only transaction".into(),
        });
    }
    if !runtime
        .has_graph(&graph)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{graph}\" does not exist"
        )));
    }

    let params = match evaluated.get(2) {
        Some(value) => age_cypher::parameter_map(value)?,
        None => BTreeMap::new(),
    };
    let (cypher_columns, cypher_rows) =
        runtime
            .run_cypher(&graph, &query, params)
            .map_err(|error| match error {
                CypherError::MissingLabelRelation(relation) => SQLError::UnknownTable(relation),
                CypherError::SerializationFailure(message) => SQLError::Routine {
                    sqlstate: "40001".into(),
                    message,
                },
                other => SQLError::Unsupported(format!("cypher: {other}")),
            })?;
    if !cypher_columns.is_empty() && cypher_columns.len() != column_aliases.len() {
        return Err(SQLError::TypeMismatch(
            "return row and column definition list do not match".into(),
        ));
    }

    let mut out = Vec::with_capacity(cypher_rows.len());
    for src in cypher_rows {
        let mut row = Vec::with_capacity(column_aliases.len());
        for (idx, target_col) in column_aliases.iter().enumerate() {
            let value = cypher_columns
                .get(idx)
                .and_then(|source_col| src.get(source_col))
                .cloned()
                .unwrap_or(Value::Null);
            let declared = column_types.get(idx).map_or("agtype", String::as_str);
            let value = age_cypher::coerce_to_column_type(value, declared, target_col)?;
            row.push(value);
        }
        out.push(row);
    }
    Ok(out)
}
