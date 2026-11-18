//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Application lookups evaluated by a trigger predicate or a rewrite rule.

use super::{StoredRule, StoredTrigger};
use crate::{
    ast::{CreateRule, CreateTrigger, Statement},
    binding::view_dependencies::{
        visit_expression_plan_json_runtime_calls, visit_expression_plan_runtime_calls,
        visit_statement_runtime_calls, StoredFunctionSite,
    },
    plan::ExpressionPlan,
    SQLError,
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl, CancellationToken};

/// Borrow the executable SQL stored by either native event catalog.
pub trait EventRuntimeCalls: Sync {
    fn runtime_definition(&self) -> EventRuntimeDefinition<'_>;
}

pub enum EventRuntimeDefinition<'a> {
    Trigger(&'a CreateTrigger),
    Rule(&'a StoredRule),
}

impl EventRuntimeCalls for StoredTrigger {
    fn runtime_definition(&self) -> EventRuntimeDefinition<'_> {
        EventRuntimeDefinition::Trigger(&self.definition)
    }
}

impl EventRuntimeCalls for StoredRule {
    fn runtime_definition(&self) -> EventRuntimeDefinition<'_> {
        EventRuntimeDefinition::Rule(self)
    }
}

impl EventRuntimeDefinition<'_> {
    /// Observe the evaluator-selected expression placements on this captured event.
    pub async fn visit_runtime_calls(
        self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
        visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
    ) -> Result<(), SQLError> {
        match self {
            Self::Trigger(trigger) => trigger.visit_runtime_calls(original, input, visit).await,
            Self::Rule(rule) => {
                let bound = rule
                    .bound_condition_plan()
                    .map(|(plan, _)| BoundCondition::Native(plan));
                rule.definition
                    .visit_rule_calls(bound, original, input, visit)
                    .await
            }
        }
    }
}

impl CreateTrigger {
    /// A trigger WHEN uses the standalone scalar evaluator. Its stored trigger routine has its own durable object identity.
    pub async fn visit_runtime_calls(
        &self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
        visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        original.check()?;
        if let Some(condition) = &self.when {
            condition
                .visit_runtime_scalar_calls(original, input, visit)
                .await?;
        }
        Ok(())
    }
}

enum BoundCondition<'a> {
    Native(&'a ExpressionPlan),
    Stored(&'a [u8]),
}

impl CreateRule {
    /// Observe a durable declaration and its optional original bound condition. An older catalog without a bound condition retains the syntax used by initial-open migration.
    pub async fn visit_persisted_runtime_calls<'a, 'input: 'a, F>(
        &'a self,
        condition_plan: Option<&'a [u8]>,
        original: &'a CancellationToken,
        input: &'a mut AsyncImageInput<'input>,
        visit: &'a mut F,
    ) -> Result<(), SQLError>
    where
        F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
    {
        self.visit_rule_calls(
            condition_plan.map(BoundCondition::Stored),
            original,
            input,
            visit,
        )
        .await
    }

    async fn visit_rule_calls<'a, 'input: 'a, F>(
        &'a self,
        bound: Option<BoundCondition<'a>>,
        original: &'a CancellationToken,
        input: &'a mut AsyncImageInput<'input>,
        visit: &'a mut F,
    ) -> Result<(), SQLError>
    where
        F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
    {
        let control = ProductionControl::new(input.memory, original, input.cancellation);
        input.checkpoint().await?;
        control.check()?;
        if let Some(expression) = &self.condition {
            match bound {
                Some(BoundCondition::Native(plan)) => {
                    visit_expression_plan_runtime_calls(plan, &control, input, visit).await?;
                }
                Some(BoundCondition::Stored(bytes)) => {
                    visit_expression_plan_json_runtime_calls(bytes, &control, input, visit).await?;
                }
                None => {
                    expression
                        .visit_runtime_query_calls(original, input, visit)
                        .await?;
                }
            }
        }
        for action in &self.actions {
            input.checkpoint().await?;
            control.check()?;
            if !matches!(action, Statement::Notify { .. }) {
                visit_statement_runtime_calls(action, &control, input, visit).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
