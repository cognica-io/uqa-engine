//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional row construction for catalog records and operator-join tuples.

use super::TableFunctionRows;
use uqa_core::Value;
use uqa_sql::{semantics::doc_id_value, SQLError};

pub(super) fn singleton_record_rows(
    value: Value,
    default_columns: &[&str],
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let columns = default_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| (*column).into())
        })
        .collect::<Vec<_>>();
    let values = match value {
        Value::Null => vec![Value::Null; columns.len()],
        Value::Record(fields) if fields.len() == columns.len() => {
            fields.into_iter().map(|(_, value)| value).collect()
        }
        Value::Record(fields) => {
            return Err(SQLError::Internal(format!(
                "sequence introspection returned {} fields for {} columns",
                fields.len(),
                columns.len()
            )));
        }
        value => {
            return Err(SQLError::Internal(format!(
                "sequence introspection returned non-record value {value:?}"
            )));
        }
    };
    Ok(TableFunctionRows::materialized(columns, vec![values]))
}

pub(super) fn operator_join_rows(
    tuples: uqa_core::GeneralizedPostingList,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let left_column = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| "left_doc_id".into());
    let right_column = column_aliases
        .get(1)
        .cloned()
        .unwrap_or_else(|| "right_doc_id".into());
    let score_column = column_aliases
        .get(2)
        .cloned()
        .unwrap_or_else(|| "_score".into());
    let mut rows = Vec::with_capacity(tuples.len());
    for tuple in tuples.entries() {
        let [left_doc_id, right_doc_id] = tuple.doc_ids.as_slice() else {
            return Err(SQLError::Internal(format!(
                "operator join produced a {}-element tuple; SQL table joins require pairs",
                tuple.doc_ids.len()
            )));
        };
        rows.push(vec![
            doc_id_value(*left_doc_id)?,
            doc_id_value(*right_doc_id)?,
            tuple
                .payload
                .fields
                .get("_score")
                .cloned()
                .unwrap_or(Value::Null),
        ]);
    }
    Ok(TableFunctionRows::materialized(
        vec![left_column, right_column, score_column],
        rows,
    ))
}
