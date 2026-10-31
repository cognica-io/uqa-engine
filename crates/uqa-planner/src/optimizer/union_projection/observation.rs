//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UNION member eligibility observes selected routines with admitted lookup scratch.

use super::{
    ComputePlan, FunctionVolatility, QueryPlan, RelationalPlan, RoutineResolution, SQLError,
    ScalarExpr, SourcePlan, VolatilityCatalog,
};
use uqa_core::memory::ProductionControl;

pub(super) fn pulled_up_member(
    query: &QueryPlan,
    routines: Option<&dyn RoutineResolution>,
    volatility: Option<&dyn VolatilityCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>, SQLError> {
    control.check()?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(Some(false));
    };
    if !query.ctes.is_empty()
        || block.r#where.is_some()
        || matches!(block.from, Some(SourcePlan::Join { .. }))
        || !matches!(block.compute, ComputePlan::Project)
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || !block.windows.is_empty()
        || !block.order_by.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.locking.is_empty()
    {
        return Ok(Some(false));
    }
    let mut effect = false;
    let mut unknown = false;
    for projection in &block.projections {
        projection.expr.try_visit(&mut |part| {
            control.check()?;
            if let ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } = part
            {
                let (call_effect, call_unknown) = function_effect(
                    name,
                    binding.as_ref(),
                    args.len(),
                    routines,
                    volatility,
                    control,
                )?;
                effect |= call_effect;
                unknown |= call_unknown;
            }
            Ok::<_, SQLError>(true)
        })?;
    }
    for query in &block.subqueries {
        match volatility.map(|catalog| {
            uqa_sql::semantics::volatility::query_contains_volatile_function_with_control(
                catalog, query, control,
            )
        }) {
            Some(Ok(volatile)) => effect |= volatile,
            Some(Err(error)) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                return Err(error)
            }
            _ => unknown = true,
        }
    }
    Ok(if effect {
        Some(false)
    } else if unknown {
        None
    } else {
        Some(true)
    })
}

fn function_effect(
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    argument_count: usize,
    routines: Option<&dyn RoutineResolution>,
    volatility: Option<&dyn VolatilityCatalog>,
    control: &ProductionControl<'_>,
) -> Result<(bool, bool), SQLError> {
    let builtin = uqa_sql::semantics::builtin_function_dispatch_name_with_control(name, control)?;
    if let Some(function) = match (routines, binding) {
        (Some(catalog), Some(binding)) => {
            uqa_sql::routines::selected_bound_function_with_control(catalog, binding, control)?
        }
        _ => None,
    } {
        return Ok((
            function.def.returns_set() || function.def.volatility == FunctionVolatility::Volatile,
            false,
        ));
    }
    if let Some(catalog) = routines {
        let table = catalog.has_registered_table_function_with_control(name, control)?;
        if table || catalog.has_registered_scalar_function_with_control(name, control)? {
            return match volatility {
                Some(catalog) => {
                    let volatile = uqa_sql::semantics::volatility::function_volatility_with_binding_and_control(
                        catalog, name, binding, argument_count, control,
                    )? == FunctionVolatility::Volatile;
                    Ok((table || volatile, false))
                }
                None => Ok((table, true)),
            };
        }
    }
    if binding.is_some_and(|binding| binding.builtin)
        || (binding.is_none() && uqa_sql::is_fixed_builtin(&builtin))
    {
        Ok((
            uqa_sql::semantics::sets::validation::builtin_returns_set(&builtin)
                || uqa_sql::semantics::volatility::builtin_function_volatility_with_control(
                    &builtin,
                    binding,
                    argument_count,
                    control,
                )? == FunctionVolatility::Volatile,
            false,
        ))
    } else {
        Ok((false, true))
    }
}
