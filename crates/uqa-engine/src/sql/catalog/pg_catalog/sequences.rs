//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::CatalogReadView;

use super::super::helpers::oids::split_schema_name;
use super::super::helpers::rows::{bool_value, row, str_value};

pub(in crate::sql::catalog) fn build_pg_sequences(
    catalog: &CatalogReadView,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = catalog
        .sequence_states()
        .into_iter()
        .map(|(name, state, role_owner)| {
            let (schema, sequence) = split_schema_name(&name)?;
            Ok(row([
                ("schemaname", str_value(schema)),
                ("sequencename", str_value(sequence)),
                ("sequenceowner", str_value(role_owner)),
                ("data_type", str_value(state.data_type.sql_name())),
                ("start_value", Value::Int(state.start)),
                ("min_value", Value::Int(state.min_value)),
                ("max_value", Value::Int(state.max_value)),
                ("increment_by", Value::Int(state.increment)),
                ("cycle", bool_value(state.cycle)),
                ("cache_size", Value::Int(state.cache_size)),
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
