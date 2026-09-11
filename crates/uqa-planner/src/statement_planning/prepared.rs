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
                value: value.clone(),
                ty: ty.sql_name(),
                bound_type: Some(ty.clone()),
                parameter_index: Some(*index),
            },
            SQLParam::Scalar(value) => ScalarExpr::Literal(value.clone()),
            SQLParam::Vector(_) | SQLParam::Tensor(_) => return,
        };
    });
}

pub mod selection;

#[cfg(test)]
mod tests;
