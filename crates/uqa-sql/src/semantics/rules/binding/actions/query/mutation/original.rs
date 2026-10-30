//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original UPDATE/DELETE qualifications retain unevaluated rule inputs in the same admitted command namespace.

use super::{OwnedMutationRuleInput, RuleMutation};
use crate::{
    ast::{BooleanTest, FunctionBinding, FunctionDispatch},
    binding::stored_routines::analysis::CatalogRoutineAnalysisContext,
    catalog::roles::RoleReference,
    plan::{
        CommandPlan, MutableCommandPlan, MutableExpressionPlan, OwnedScalarExpr, ScalarQueryArena,
    },
    production::Buffer,
    semantics::rules::{
        analysis::{rule_returning_columns_with_control, RuleAnalysisContext},
        binding::OwnedRuleQueryResolver,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl RuleMutation<'_> {
    /// Bind each conditional INSTEAD predicate without evaluating NEW assignments. Stored SQL errors remain lazy; admission and cancellation failures abort production immediately.
    pub fn original_condition_with_control(
        self,
        context: RuleAnalysisContext<'_>,
        binding: CatalogRoutineAnalysisContext<'_>,
        subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExpressionPlan>, SQLError> {
        control.check()?;
        let rules =
            context
                .rules
                .capture_rules_with_control(self.table(), self.event(), control)?;
        let definitions = rule_returning_columns_with_control(context, self.table(), control)?;
        let mut resolver = OwnedRuleQueryResolver::from_mutation_with_control(
            context,
            self,
            &definitions,
            None,
            None,
            control,
            Ok,
        )?;
        let mut predicates = Buffer::new(control);
        let mut arena = ScalarQueryArena::copy_from(&[], control)?;
        for rule in rules.iter() {
            control.check()?;
            let rule = rule?;
            if !rule.definition.instead {
                continue;
            }
            let Some(condition) =
                resolver.condition_with_control(binding, rule, &definitions, subject, control)?
            else {
                continue;
            };
            let function = FunctionBinding::dispatched_with_control(
                FunctionDispatch::BooleanTest(BooleanTest::IsNotTrue),
                control,
            )?;
            let predicate = OwnedScalarExpr::call(
                control.copy_text(&function.name)?,
                Some(function),
                std::iter::once(arena.append_expression(condition, control)?),
                control,
            )?;
            predicates.push(predicate, control)?;
        }
        if predicates.is_empty() {
            return Ok(None);
        }
        Ok(Some(MutableExpressionPlan::from_parts(
            OwnedScalarExpr::boolean(predicates.into_iter(), true, control)?,
            arena,
            control,
        )?))
    }
}

impl OwnedMutationRuleInput {
    /// A suppressing rule retires the original command before resolving its assignments. Conditional rules append one private predicate arena to the surviving command.
    pub fn qualify_original(
        context: RuleAnalysisContext<'_>,
        statement: MutableCommandPlan,
        binding: CatalogRoutineAnalysisContext<'_>,
        subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableCommandPlan>, SQLError> {
        control.check()?;
        let input = match &*statement {
            CommandPlan::Update(plan) => RuleMutation::Update(plan),
            CommandPlan::Delete(plan) => RuleMutation::Delete(plan),
            _ => {
                return Err(SQLError::Internal(
                    "original qualification requires UPDATE or DELETE".into(),
                ))
            }
        };
        let rules =
            context
                .rules
                .capture_rules_with_control(input.table(), input.event(), control)?;
        if rules.any(|rule| rule.definition.instead && rule.definition.condition.is_none())? {
            return Ok(None);
        }
        let condition =
            input.original_condition_with_control(context, binding, subject, control)?;
        let Some(condition) = condition else {
            return Ok(Some(statement));
        };
        Ok(Some(statement.map_predicate_input(
            control,
            |previous, arena| {
                let condition = arena.append_expression(condition, control)?;
                Ok(Some(match previous {
                    Some(previous) => {
                        OwnedScalarExpr::boolean([previous, condition].into_iter(), true, control)?
                    }
                    None => condition,
                }))
            },
        )?))
    }
}
