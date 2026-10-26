//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE scheduling borrows the native SQL analysis under the invoking query's allowance.

use super::CteExecutionContext;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    plan::QueryPlan,
    semantics::{cte_strategy::ScheduledCte, volatility::VolatilityCatalog},
    SQLError,
};

pub(crate) fn schedule_plan_ctes<'p, S: Clone>(
    context: CteExecutionContext<'_, S>,
    catalog: &dyn VolatilityCatalog,
    plan: &'p QueryPlan,
) -> Result<Produced<Vec<ScheduledCte<'p>>>, SQLError> {
    let retained = context.queries.read_control()?;
    uqa_sql::semantics::cte_strategy::schedule_plan_ctes_with_control(
        catalog,
        plan,
        ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            context.runtime.cancellation,
        ),
    )
}
