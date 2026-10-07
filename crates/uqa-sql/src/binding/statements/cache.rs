//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reusable SQL analysis keeps parameter types and converted immutable inputs, never invocation values or optimizer choices.

use super::{analyze_reusable_inputs, StatementAnalysisContext};
use crate::{plan::UnifiedPlan, ColumnType, RowSchema, SQLError, SQLParam, ScalarExpr};

#[derive(Clone, PartialEq)]
struct ParameterShape {
    declared: bool,
    resolved: Option<ColumnType>,
}

fn parameter_shapes(params: &[SQLParam]) -> Option<Vec<ParameterShape>> {
    let schema = RowSchema::default();
    params
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            Some(ParameterShape {
                declared: parameter.declared_scalar_type().is_some(),
                resolved: crate::scalar_type(&ScalarExpr::Param(index + 1), &schema, params)
                    .ok()?,
            })
        })
        .collect()
}

/// Analyzed syntax with immutable input conversions and no optimized access paths. A caller must validate the original statement, catalog, namespace, authority and parser settings in the selected statement snapshot before offering this entry for reuse. Parameter values remain supplied by each execution; their inferred types and explicit declarations must still match.
#[derive(Clone)]
pub struct AnalyzedStatement {
    plan: UnifiedPlan,
    parameters: Vec<ParameterShape>,
}

impl AnalyzedStatement {
    pub fn plan_for(&self, params: &[SQLParam]) -> Option<&UnifiedPlan> {
        (parameter_shapes(params).as_ref() == Some(&self.parameters)).then_some(&self.plan)
    }
}

/// Analyze under the current caller's scope, then retain only inputs whose conversion lifetime spans ordinary messages. A parameter whose shape cannot be captured disables reuse without changing the original analysis's result or diagnostic order.
pub fn analyze_for_statement_reuse(
    context: &StatementAnalysisContext<'_>,
    mut plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<(UnifiedPlan, Option<AnalyzedStatement>), SQLError> {
    let (_, reusable) = analyze_reusable_inputs(context, &mut plan, params)?;
    let retained = reusable
        .then(|| parameter_shapes(params))
        .flatten()
        .map(|parameters| AnalyzedStatement {
            plan: plan.clone(),
            parameters,
        });
    Ok((plan, retained))
}
