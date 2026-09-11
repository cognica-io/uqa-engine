//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cancellation and SQL validation before any command-specific state is observed.

use super::context::StatementValidationContext;
use uqa_core::CancellationToken;
use uqa_sql::{plan::UnifiedPlan, SQLError};

pub fn validate_plan(
    context: &StatementValidationContext<'_>,
    cancellation: &CancellationToken,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    cancellation.check()?;
    {
        let resolution = context.session.relation_name_resolution();
        uqa_sql::semantics::cte_validation::validate_plan(
            &uqa_sql::semantics::cte_validation::CteValidationContext {
                catalog: context.rules,
                resolution: &resolution,
            },
            plan,
        )?;
    }
    super::transactions::validate_transaction_plan(
        context.transactions,
        &context.effects.query_effect_context(),
        plan,
    )
}

#[cfg(test)]
mod tests;
