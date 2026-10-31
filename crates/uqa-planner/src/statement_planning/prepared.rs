//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Custom-versus-generic plan selection and type-preserving parameter specialization.

#[derive(Clone, Copy)]
pub struct PreparedPlanUsage {
    pub has_parameters: bool,
    pub custom_plans: i64,
    pub total_custom_cost: f64,
}

pub fn choose_custom_plan(usage: PreparedPlanUsage, mode: &str, generic_cost: Option<f64>) -> bool {
    if !usage.has_parameters {
        return false;
    }
    match mode {
        "force_generic_plan" => false,
        "force_custom_plan" => true,
        _ if usage.custom_plans < 5 => true,
        _ => generic_cost
            .is_some_and(|cost| cost >= usage.total_custom_cost / usage.custom_plans as f64),
    }
}

pub fn specialize_parameters(plan: &mut crate::UnifiedPlan, parameters: &[uqa_sql::SQLParam]) {
    use uqa_sql::SQLParam;
    use uqa_sql::ScalarExpr;
    plan.rewrite_scalar_expressions(&mut |expression| {
        let ScalarExpr::Param(index) = expression else {
            return;
        };
        let Some(parameter) = index.checked_sub(1).and_then(|index| parameters.get(index)) else {
            return;
        };
        *expression = match parameter {
            SQLParam::TypedScalar { value, ty } => ScalarExpr::TypedLiteral {
                collation_oid: None,
                composite_source: None,
                value: value.clone(),
                ty: ty.catalog_name(),
                bound_type: Some(ty.clone()),
                parameter_index: Some(*index),
            },
            SQLParam::Scalar(value) => crate::optimizer::retain_computed_integer(
                ScalarExpr::Literal(value.clone()),
                Some(*index),
            ),
            SQLParam::Vector(_)
            | SQLParam::Tensor(_)
            | SQLParam::TypedTensor(_)
            | SQLParam::DeferredError { .. } => return,
        };
    });
}

/// Specialize a private executable without separating parameter payloads from their producing allowance.
pub fn specialize_parameters_owned(
    plan: uqa_sql::plan::MutableExecutablePlan,
    parameters: &[uqa_sql::SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_sql::plan::MutableExecutablePlan, uqa_sql::SQLError> {
    use uqa_sql::{plan::OwnedScalarExpr, SQLParam, ScalarExpr};
    plan.transform_scalar_nodes(control, &mut |_, expression| {
        let ScalarExpr::Param(index) = &*expression else {
            return Ok(expression);
        };
        let Some(parameter) = index.checked_sub(1).and_then(|index| parameters.get(index)) else {
            return Ok(expression);
        };
        match parameter {
            SQLParam::TypedScalar { value, ty } => Ok(OwnedScalarExpr::typed_literal(
                control.copy_value(value)?,
                ty.catalog_name_with_control(control)?,
                Some(ty.clone_with_control(control)?),
                None,
                Some(*index),
                control,
            )?),
            SQLParam::Scalar(value) => crate::optimizer::retain_computed_integer_owned(
                OwnedScalarExpr::literal(control.copy_value(value)?, control)?,
                Some(*index),
                control,
            ),
            SQLParam::Vector(_)
            | SQLParam::Tensor(_)
            | SQLParam::TypedTensor(_)
            | SQLParam::DeferredError { .. } => Ok(expression),
        }
    })
}

pub mod selection;

#[cfg(test)]
mod tests;
