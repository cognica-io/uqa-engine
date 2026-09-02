//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, SessionExecutionView};

use super::super::helpers::rows::{bool_value, row, str_value};

pub(in crate::sql::catalog) fn build_pg_sequences(
    catalog: &CatalogReadView,
    session: SessionExecutionView<'_>,
) -> Result<Vec<ResultRow>, SQLError> {
    let temporary_schema = session.temporary_schema_name();
    let current_user = session.current_user();
    let mut rows = catalog
        .sequence_states()
        .into_iter()
        .filter(|(relation, _, persistence, _)| {
            *persistence != uqa_sql::ast::RelationPersistence::Temporary
                || relation.schema == temporary_schema
        })
        .map(|(relation, state, _, security)| {
            row([
                ("schemaname", str_value(relation.schema)),
                ("sequencename", str_value(relation.name)),
                ("sequenceowner", str_value(security.role_owner.clone())),
                ("data_type", str_value(state.data_type.sql_name())),
                ("start_value", Value::Int(state.start)),
                ("min_value", Value::Int(state.min_value)),
                ("max_value", Value::Int(state.max_value)),
                ("increment_by", Value::Int(state.increment)),
                ("cycle", bool_value(state.cycle)),
                ("cache_size", Value::Int(state.cache_size)),
                (
                    "last_value",
                    if state.called
                        && catalog.sequence_value_is_readable_to(&security, &current_user)
                    {
                        Value::Int(state.current)
                    } else {
                        Value::Null
                    },
                ),
            ])
        })
        .collect::<Vec<_>>();
    rows.extend(super::super::ag_catalog::age_pg_sequences_rows(catalog)?);
    Ok(rows)
}
