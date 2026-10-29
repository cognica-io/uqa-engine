//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reusable SQL analysis keeps parameter types and converted immutable inputs, never invocation values or optimizer choices.

use super::{analyze_reusable_inputs, StatementAnalysisContext};
use crate::{
    plan::UnifiedPlan, schema::ColumnTypeSchema, ColumnType, SQLError, SQLParam, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[derive(Clone, PartialEq)]
struct ParameterShape {
    declared: bool,
    resolved: Option<ColumnType>,
}

fn parameter_shapes(
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<ParameterShape>>>, SQLError> {
    let schema = ColumnTypeSchema::new(&[]);
    let mut shapes = ProductionVec::new(*control);
    shapes.reserve(params.len())?;
    for (index, parameter) in params.iter().enumerate() {
        let resolved = match crate::scalar_type_with_control(
            &ScalarExpr::Param(index + 1),
            &schema,
            params,
            control,
        ) {
            Ok(resolved) => resolved,
            Err(error)
                if error.sqlstate() == Some("53200")
                    || matches!(error.primary(), SQLError::Cancelled(_)) =>
            {
                return Err(error)
            }
            Err(_) => return Ok(None),
        };
        let (resolved, memory) = match resolved {
            Some(resolved) => {
                let (ty, memory) = resolved.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        shapes.push_produced(control.finish(
            ParameterShape {
                declared: parameter.declared_scalar_type().is_some(),
                resolved,
            },
            memory,
        )?)?;
    }
    Ok(Some(shapes.finish()?))
}

/// Analyzed syntax with immutable input conversions and no optimized access paths. A caller must validate the original statement, catalog, namespace, authority and parser settings in the selected statement snapshot before offering this entry for reuse. Parameter values remain supplied by each execution; their inferred types and explicit declarations must still match.
#[derive(Clone)]
pub struct AnalyzedStatement(Arc<Produced<AnalyzedStorage>>);

struct AnalyzedStorage {
    plan: UnifiedPlan,
    parameters: Vec<ParameterShape>,
}

impl AnalyzedStatement {
    pub fn plan_for(&self, params: &[SQLParam]) -> Option<&UnifiedPlan> {
        let shapes = parameter_shapes(params, &ProductionControl::uncontrolled()).ok()??;
        (*shapes == self.0.parameters).then_some(&self.0.plan)
    }

    /// Retain complete analyzed syntax and parameter declarations under the invoking request's allowance. All destinations are admitted before allocation; copying a published handle shares its immutable storage and lease.
    pub fn capture_with_control(
        plan: &UnifiedPlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        let Some(parameters) = parameter_shapes(params, control)? else {
            return Ok(None);
        };
        let plan = plan.clone_with_control(control)?;
        let (plan, memory) = plan.into_parts();
        let (parameters, parameter_memory) = parameters.into_parts();
        let storage = control.finish(
            AnalyzedStorage { plan, parameters },
            control.combine(memory, parameter_memory),
        )?;
        Ok(Some(Self(storage.into_shared(control)?)))
    }

    pub fn reserved_bytes(&self) -> usize {
        self.0.reserved_bytes()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Analyze under the current caller's scope, then retain only inputs whose conversion lifetime spans ordinary messages. A parameter whose shape cannot be captured disables reuse without changing the original analysis's result or diagnostic order.
pub fn analyze_for_statement_reuse(
    context: &StatementAnalysisContext<'_>,
    mut plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<(UnifiedPlan, Option<AnalyzedStatement>), SQLError> {
    let (_, reusable) = analyze_reusable_inputs(context, &mut plan, params)?;
    let retained = if reusable {
        AnalyzedStatement::capture_with_control(&plan, params, &ProductionControl::uncontrolled())?
    } else {
        None
    };
    Ok((plan, retained))
}
