//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse ordinary statement analysis only after pinning and every writer refresh; optimize against the current data on every call.

use super::context::BatchExecutionContext;
use uqa_sql::{plan::UnifiedPlan, SQLError, SQLParam};

pub(super) enum StatementPlan {
    Prepared(uqa_sql::plan::SharedExecutablePlan),
    Optimized(uqa_sql::plan::SharedExecutablePlan),
}

impl std::ops::Deref for StatementPlan {
    type Target = UnifiedPlan;
    fn deref(&self) -> &UnifiedPlan {
        match self {
            Self::Prepared(plan) | Self::Optimized(plan) => plan,
        }
    }
}

pub(super) fn plan<S: Clone + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    logical: uqa_sql::plan::SharedExecutablePlan,
    params: &[SQLParam],
    cacheable: bool,
    prepared: bool,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<StatementPlan, SQLError> {
    if prepared {
        // Prepared inference, dependency checks and optimization belong to its retained definition.
        return Ok(StatementPlan::Prepared(logical));
    }
    if !cacheable {
        return context
            .planning
            .plan_owned_for_execution(logical.copy_for_rewrite(control)?, params, control)
            .map(StatementPlan::Optimized);
    }
    let cached = context.cache.cached_sql_analysis(sql);
    let (plan, analyzed) = context.planning.plan_owned_with_cached_analysis(
        logical.copy_for_rewrite(control)?,
        params,
        cached,
        control,
    )?;
    context.cache.cache_sql_analysis(sql, analyzed, control)?;
    Ok(StatementPlan::Optimized(plan))
}

pub(in crate::statement) fn lower(
    statement: &uqa_sql::Statement,
    aggregates: &dyn uqa_sql::plan::AggregateClassifier,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
    UnifiedPlan::lower_borrowed_for_rewrite(statement, aggregates, control)?
        .finish(control)?
        .into_shared(control)
}
