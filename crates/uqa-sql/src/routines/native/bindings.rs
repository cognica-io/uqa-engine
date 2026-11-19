//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured routines retain SQL's original expression and result contracts without executing bodies or catalog input functions.

use crate::{
    ast::FunctionBody,
    catalog::scalar_bindings::{expression_type_with_work, NativeScalarContext},
    plan::{PlanRef, UnifiedPlan},
    routines::{
        body_parameters::sql_body_parameter_scope_with_control,
        body_validation::routine_parameter_values_with_control, declaration::RoutineTypeCatalog,
        result_check::sql_function_result_layout_with_control, CompiledFunctionBody, RoutineBody,
        SQLUserFunction,
    },
    type_resolution::assignment_type_compatible_with_control,
    RowSchema, SQLError,
};
use uqa_core::memory::ProductionControl;

/// Check original declarations and retained executable statements independently. Deferred source bodies remain uncompiled, while defaults always have their own empty row and parameter scope.
pub fn validate_captured_bindings(
    routine: &SQLUserFunction,
    types: &dyn RoutineTypeCatalog,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate_captured_bindings_with_work(
        routine,
        || types,
        context,
        control,
        &mut (),
    ))
}

pub async fn validate_captured_bindings_with_work<'a>(
    routine: &SQLUserFunction,
    types: impl Fn() -> &'a dyn RoutineTypeCatalog,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    control.check()?;
    let definition = &routine.def;
    for parameter in &definition.params {
        work.checkpoint().await?;
        control.check()?;
        if let Some(default) = &parameter.default {
            let actual =
                expression_type_with_work(default, &RowSchema::default(), context, control, work)
                    .await?;
            let expected = types()
                .resolve_catalog_column_type_name_with_control(&parameter.type_name, control)?;
            if let Some(actual) = actual {
                if !assignment_type_compatible_with_control(&actual, &expected, control)? {
                    return Err(SQLError::Internal(
                        "invalid native routine default type".into(),
                    ));
                }
            }
        }
    }
    let FunctionBody::Statements(statements) = &definition.body else {
        return Ok(());
    };
    let RoutineBody::Bound(body) = &routine.body else {
        return Err(SQLError::Internal(
            "native routine has no bound body".into(),
        ));
    };
    let CompiledFunctionBody::SQL(plans) = &***body else {
        return Err(SQLError::Internal(
            "native routine has a session body".into(),
        ));
    };
    let params = routine_parameter_values_with_control(types(), definition, control)?;
    let parameters = sql_body_parameter_scope_with_control(definition, &params, control)?;
    let inspect = |plan: PlanRef<'_>, original: bool| {
        crate::binding::analyze_stored_routine_statement(
            context.routines,
            plan,
            &params,
            original.then_some(&parameters),
            context.binding.copy_with_control(control)?,
            control,
        )
    };
    let mut last = None;
    for statement in statements {
        control.check()?;
        let plan = UnifiedPlan::lower_stored_statement_with_work(statement, control, work).await?;
        last = Some(inspect(plan.as_plan_ref(), true)?);
        plan.retire(control, work).await?;
    }
    sql_function_result_layout_with_control(types(), definition, last.as_ref(), control)?;
    last = None;
    for plan in plans {
        work.checkpoint().await?;
        control.check()?;
        last = Some(inspect(plan.into(), false)?);
    }
    sql_function_result_layout_with_control(types(), definition, last.as_ref(), control)?;
    Ok(control.check()?)
}

#[cfg(test)]
mod tests;
