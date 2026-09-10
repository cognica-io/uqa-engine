//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current-session prepared statement definitions and plan selection counters.

use super::helpers::rows::{catalog_array, row};
use crate::engine_capabilities::SessionExecutionView;
use uqa_core::{TemporalValue, Value};
use uqa_sql::{ColumnType, ResultRow, SQLError};

pub(super) fn rows(session: SessionExecutionView<'_>) -> Result<Vec<ResultRow>, SQLError> {
    session
        .prepared_statements()
        .into_iter()
        .map(|entry| {
            let parameters = type_array(entry.parameter_types.iter())?;
            let results = entry
                .result_types
                .as_ref()
                .map(|types| type_array(types.iter()))
                .transpose()?
                .unwrap_or(Value::Null);
            Ok(row([
                ("name", Value::Str(entry.name)),
                (
                    "statement",
                    entry
                        .source_sql
                        .map_or(Value::Null, |source| Value::Str(source.to_string())),
                ),
                (
                    "prepare_time",
                    Value::Temporal(TemporalValue::TimestampTz {
                        micros: entry.prepared_at_micros,
                    }),
                ),
                ("parameter_types", parameters),
                ("result_types", results),
                ("from_sql", Value::Bool(entry.from_sql)),
                ("generic_plans", Value::Int(entry.generic_plans)),
                ("custom_plans", Value::Int(entry.custom_plans)),
            ]))
        })
        .collect()
}

fn type_array<'a>(types: impl Iterator<Item = &'a Option<ColumnType>>) -> Result<Value, SQLError> {
    catalog_array(
        types
            .map(|ty| {
                ty.as_ref().map_or(Value::Null, |ty| {
                    let oid = match ty {
                        ColumnType::Domain { oid, .. } => *oid,
                        _ => super::postgres_result_type(ty).type_oid,
                    };
                    Value::Int(i64::from(oid))
                })
            })
            .collect(),
        "prepared statement types",
    )
}
