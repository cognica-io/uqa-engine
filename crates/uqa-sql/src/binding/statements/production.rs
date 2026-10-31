//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement analysis consumes its destination and preserves the invoking production owner.

use super::{AnalyzedResult, StatementAnalysisContext};
use crate::{
    binding::production::ControlledBindingContext,
    plan::{CommandPlan, MutableExecutablePlan, PlanRef},
    SQLError, SQLParam,
};
use uqa_core::memory::ProductionControl;

pub fn analyze_reusable_inputs_owned(
    context: &StatementAnalysisContext<'_>,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<(MutableExecutablePlan, AnalyzedResult, bool), SQLError> {
    let mut input = Some(plan);
    let mut output = None;
    context
        .scopes
        .with_controlled_scope(control, &mut |binding| {
            let plan = input
                .take()
                .ok_or_else(|| SQLError::Internal("statement analysis scope ran twice".into()))?;
            if matches!(
                plan.as_plan_ref(),
                PlanRef::Command(CommandPlan::Explain { .. })
            ) {
                let mut plan = Some(plan);
                context
                    .scopes
                    .with_controlled_scope(control, &mut |binding| {
                        output = Some(analyze(
                            context,
                            plan.take().ok_or_else(|| {
                                SQLError::Internal("EXPLAIN analysis scope ran twice".into())
                            })?,
                            params,
                            binding,
                            control,
                        )?);
                        Ok(())
                    })?;
            } else {
                output = Some(analyze(context, plan, params, binding, control)?);
            }
            Ok(())
        })?;
    output.ok_or_else(|| SQLError::Internal("statement analysis scope did not run".into()))
}

fn analyze(
    context: &StatementAnalysisContext<'_>,
    plan: MutableExecutablePlan,
    params: &[SQLParam],
    binding: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<(MutableExecutablePlan, AnalyzedResult, bool), SQLError> {
    if let PlanRef::Command(CommandPlan::Explain { options, .. }) = plan.as_plan_ref() {
        let infer = params.is_empty() && options.infers_unbound_parameters()?;
        let MutableExecutablePlan::Command(command) = plan else {
            unreachable!()
        };
        let mut reusable = true;
        let mut parameters = None;
        let command = command
            .map_executable_body(control, |body| {
                if infer {
                    let (body, values, body_reusable) =
                        super::super::preparation::read_generic_explain_inputs_owned(
                            context.routines,
                            body,
                            binding,
                            context.aliases,
                            control,
                        )?;
                    parameters = Some(values);
                    reusable &= body_reusable;
                    Ok(body)
                } else {
                    let (body, _, body_reusable) =
                        analyze(context, body, params, binding, control)?;
                    reusable &= body_reusable;
                    Ok(body)
                }
            })?
            .set_explain_parameters(parameters, control)?;
        let CommandPlan::Explain { options, .. } = &*command else {
            unreachable!()
        };
        let schema = options.result_schema_with_control(control)?;
        return Ok((
            MutableExecutablePlan::Command(command),
            AnalyzedResult::Schema(schema),
            reusable,
        ));
    }
    let input = super::super::preparation::read_executable_inputs_owned(
        context.routines,
        plan,
        params,
        binding,
        context.aliases,
        control,
    )?;
    Ok((input.plan, input.result, input.reusable))
}
