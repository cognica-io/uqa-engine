//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current-session cursor descriptions, including a portal executing its own query.

use super::helpers::rows::row;
use crate::catalog::services::CatalogSession;
use uqa_core::{TemporalValue, Value};
use uqa_sql::ResultRow;

pub fn rows(session: &dyn CatalogSession) -> Vec<ResultRow> {
    session
        .cursors()
        .into_iter()
        .map(|entry| {
            row([
                ("name", Value::Str(entry.name)),
                (
                    "statement",
                    entry
                        .source_sql
                        .map_or(Value::Null, |source| Value::Str(source.to_string())),
                ),
                ("is_holdable", Value::Bool(entry.is_holdable)),
                ("is_binary", Value::Bool(entry.is_binary)),
                ("is_scrollable", Value::Bool(entry.is_scrollable)),
                (
                    "creation_time",
                    Value::Temporal(TemporalValue::TimestampTz {
                        micros: entry.created_at_micros,
                    }),
                ),
            ])
        })
        .collect()
}
