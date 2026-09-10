//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statement metadata for SQL-owned correlation analysis.

use crate::Engine;
use uqa_sql::binding::correlation::{CorrelationContext, DecorrelatedExistsPlan};
use uqa_sql::{plan::QueryPlan, SQLError};

pub(super) fn decorrelate_exists(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<Option<DecorrelatedExistsPlan>, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    uqa_sql::binding::correlation::decorrelate_exists(
        CorrelationContext {
            catalog: &catalog,
            resolution: &resolution,
        },
        plan,
    )
}

pub(super) fn query_depends_on_outer_row(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    uqa_sql::binding::correlation::query_depends_on_outer_row(
        CorrelationContext {
            catalog: &catalog,
            resolution: &resolution,
        },
        plan,
    )
}
