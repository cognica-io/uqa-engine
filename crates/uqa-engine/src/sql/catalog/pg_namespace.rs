//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capability-scoped `pg_namespace` row synthesis.

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use uqa_sql::{ResultRow, SQLError};

use super::helpers::oids::{current_user_oid, schema_oid};
use super::helpers::rows::{int_value, row, str_value};
use super::helpers::views::all_schema_names;

pub(super) fn build_pg_namespace(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    Ok(all_schema_names(catalog, resolution)?
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
