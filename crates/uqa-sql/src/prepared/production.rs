//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared declarations and analysis parameters admit their independently retained buffers and type payloads.

use crate::{
    plan::{PlanRef, UnifiedPlan},
    ColumnType, FunctionTypeResolver, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub fn declared_parameter_types_with_control(
    resolver: &dyn FunctionTypeResolver,
    plan: &UnifiedPlan,
    declared: &[ColumnType],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    declared_parameter_types_for_plan_with_control(resolver, plan.into(), declared, control)
}

pub fn declared_parameter_types_for_plan_with_control(
    resolver: &dyn FunctionTypeResolver,
    plan: PlanRef<'_>,
    declared: &[ColumnType],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for ty in declared {
        control.check()?;
        if let ColumnType::Named(name) = ty {
            if is_unknown_type(name, control)? {
                output.push_produced(control.finish(None, control.empty_reservation())?)?;
                continue;
            }
        }
        let (ty, memory) = crate::type_resolution::resolve_declared_column_type_with_control(
            resolver, ty, control,
        )?
        .into_parts();
        output.push_produced(control.finish(Some(ty), memory)?)?;
    }
    if super::utility::requires_analysis_ref(plan) {
        let mut status = Ok(());
        let mut visit = |root: &ScalarExpr| {
            if status.is_ok() {
                status = root.try_visit(&mut |expression| {
                    control.check()?;
                    if let ScalarExpr::Param(index) = expression {
                        while output.len() < *index {
                            output.push_produced(
                                control.finish(None, control.empty_reservation())?,
                            )?;
                        }
                    }
                    Ok::<_, SQLError>(true)
                });
            }
        };
        match plan {
            PlanRef::Query(query) => query.visit_scalar_expressions(&mut visit),
            PlanRef::Command(command) => command.visit_scalar_expressions(&mut visit),
        }
        status?;
    }
    Ok(output.finish()?)
}

pub(super) fn is_unknown_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(
        crate::parse_regtype_name_with_control(name, control)?.is_some_and(|parsed| {
            parsed.array_dimensions == 0
                && !parsed.has_type_modifiers
                && match parsed.names.as_slice() {
                    [local] => local == "unknown",
                    [schema, local] => schema == "pg_catalog" && local == "unknown",
                    _ => false,
                }
        }),
    )
}

pub fn analysis_parameters_with_control(
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<SQLParam>>, SQLError> {
    let mut parameters = ProductionVec::new(*control);
    parameters.reserve(types.len())?;
    for ty in types {
        control.check()?;
        let parameter = if let Some(ty) = ty {
            let (ty, memory) = ty.clone_with_control(control)?.into_parts();
            control.finish(SQLParam::typed_scalar(Value::Null, ty), memory)?
        } else {
            control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?
        };
        parameters.push_produced(parameter)?;
    }
    Ok(parameters.finish()?)
}
