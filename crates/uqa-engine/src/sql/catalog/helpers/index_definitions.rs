//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index catalog decoding and definition rendering.

use uqa_sql::SQLError;

pub(in crate::sql::catalog) fn index_columns(columns_json: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(columns_json)
        .map_err(|err| SQLError::Internal(format!("decode index column catalog: {err}")))
}

pub(in crate::sql::catalog) fn indexdef(
    name: &str,
    index_type: &str,
    table: &str,
    columns: &[String],
) -> String {
    let method = if index_type.is_empty() {
        "btree"
    } else {
        index_type
    };
    format!(
        "CREATE INDEX {} ON {table} USING {method} ({})",
        uqa_sql::expr::quote_ident(name),
        columns
            .iter()
            .map(|column| uqa_sql::expr::quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
