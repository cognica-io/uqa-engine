//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capability-scoped `pg_namespace` row synthesis.

use crate::engine_capabilities::{CatalogReadView, SessionExecutionView};
use uqa_sql::{ResultRow, SQLError};

use super::helpers::{all_schema_names, current_user_oid, int_value, row, schema_oid, str_value};

pub(super) fn build_pg_namespace(
    catalog: CatalogReadView<'_>,
    session: SessionExecutionView<'_>,
) -> Result<Vec<ResultRow>, SQLError> {
    Ok(all_schema_names(catalog, session)?
        .into_iter()
        .map(|schema| {
            row([
                ("oid", int_value(schema_oid(&schema))),
                ("nspname", str_value(schema)),
                ("nspowner", int_value(current_user_oid())),
                ("nspacl", uqa_core::Value::Null),
            ])
        })
        .collect())
}
