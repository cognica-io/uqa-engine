//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture catalog and name resolution for SQL-owned correlation analysis.

use super::SubqueryServices;
use uqa_sql::binding::correlation::{CorrelationContext, DecorrelatedExistsPlan};
use uqa_sql::{plan::QueryPlan, SQLError};

pub(super) fn decorrelate_exists<S: Clone + 'static>(
    services: &SubqueryServices<'_, S>,
    plan: &QueryPlan,
) -> Result<Option<DecorrelatedExistsPlan>, SQLError> {
    let catalog = services.catalog.catalog_snapshot();
    let resolution = services.session.relation_name_resolution();
    uqa_sql::binding::correlation::decorrelate_exists(
        CorrelationContext {
            catalog: &catalog,
            resolution: &resolution,
        },
        plan,
    )
}

pub(super) fn query_depends_on_outer_row<S: Clone + 'static>(
    services: &SubqueryServices<'_, S>,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    let catalog = services.catalog.catalog_snapshot();
    let resolution = services.session.relation_name_resolution();
    uqa_sql::binding::correlation::query_depends_on_outer_row(
        CorrelationContext {
            catalog: &catalog,
            resolution: &resolution,
        },
        plan,
    )
}
