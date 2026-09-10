//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt immutable engine catalog and CTE snapshots to SQL binding inputs.

use crate::query::CteScope;
use std::sync::Arc;
use uqa_sql::binding::context::BindingContext;
use uqa_sql::SQLError;

pub fn binding_context<S: Clone>(ctes: &CteScope<S>) -> Result<BindingContext<'_>, SQLError> {
    Ok(BindingContext {
        catalog: Arc::new(ctes.catalog_read_view()?),
        resolution: ctes.relation_name_resolution()?,
        ctes: ctes
            .rows
            .iter()
            .map(|(name, rows)| {
                let schema = rows.row_schema();
                let schema = ctes.recursive_control_width(name).map_or_else(
                    || schema.clone(),
                    |visible| uqa_sql::binding::hide_recursive_generated_schema(schema, visible),
                );
                (name.clone(), schema)
            })
            .collect(),
        deferred_ctes: ctes.deferred_ctes().clone(),
        non_returning_ctes: ctes.non_returning_ctes.clone(),
        scalar_subqueries: &ctes.scalar_subqueries,
    })
}
