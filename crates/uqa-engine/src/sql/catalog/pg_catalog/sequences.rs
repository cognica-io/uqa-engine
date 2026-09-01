//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::CatalogReadView;

use super::super::helpers::oids::{current_user_name, split_schema_name};
use super::super::helpers::rows::{bool_value, row, str_value};

pub(in crate::sql::catalog) fn build_pg_sequences(
    catalog: &CatalogReadView,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = catalog
        .sequence_states()
        .into_iter()
        .map(|(name, state)| {
            let (schema, sequence) = split_schema_name(&name)?;
            let ascending = state.increment > 0;
            Ok(row([
                ("schemaname", str_value(schema)),
                ("sequencename", str_value(sequence)),
                ("sequenceowner", str_value(current_user_name())),
                ("data_type", str_value("bigint")),
                ("start_value", Value::Int(state.start)),
                (
                    "min_value",
                    Value::Int(if ascending { 1 } else { i64::MIN }),
                ),
                (
                    "max_value",
                    Value::Int(if ascending { i64::MAX } else { -1 }),
                ),
                ("increment_by", Value::Int(state.increment)),
                ("cycle", bool_value(false)),
                ("cache_size", Value::Int(1)),
                (
                    "last_value",
                    if state.called {
                        Value::Int(state.current)
                    } else {
                        Value::Null
                    },
                ),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(super::super::ag_catalog::age_pg_sequences_rows(catalog)?);
    Ok(rows)
}
