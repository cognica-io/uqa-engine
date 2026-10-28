//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse ordinary statement analysis only after pinning and every writer refresh; optimize against the current data on every call.

use super::context::BatchExecutionContext;
use uqa_sql::{plan::UnifiedPlan, SQLError, SQLParam};

pub(super) enum StatementPlan {
    Prepared(UnifiedPlan),
    Optimized(uqa_sql::plan::SharedExecutablePlan),
}

impl std::ops::Deref for StatementPlan {
    type Target = UnifiedPlan;
    fn deref(&self) -> &UnifiedPlan {
        match self {
            Self::Prepared(plan) => plan,
            Self::Optimized(plan) => plan,
        }
    }
}

pub(super) fn plan<S: Clone + 'static>(
    context: &BatchExecutionContext<'_, S>,
    sql: &str,
    logical: UnifiedPlan,
    params: &[SQLParam],
    cacheable: bool,
    prepared: bool,
) -> Result<StatementPlan, SQLError> {
    if prepared {
        // Prepared inference, dependency checks and optimization belong to its retained definition.
        return Ok(StatementPlan::Prepared(logical));
    }
    if !cacheable {
        return context
            .planning
            .plan_for_execution(logical, params)
            .map(StatementPlan::Optimized);
    }
    let cached = context.cache.cached_sql_analysis(sql);
    let (plan, analyzed) = context
        .planning
        .plan_with_cached_analysis(logical, params, cached)?;
    context.cache.cache_sql_analysis(sql, analyzed);
    Ok(StatementPlan::Optimized(plan))
}
