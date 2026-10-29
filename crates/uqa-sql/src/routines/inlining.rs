//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-plan expansion of selected SQL routines. Source text is analyzed for
//! each planning attempt; declining expansion never changes the session body cache.

mod operators;
mod owned;
mod owned_arguments;
mod owned_substitution;
mod preparation;
mod properties;
mod selection;
pub use selection::InlineOwnedRoutineExpression;
mod table;
pub use table::{InlineOwnedRoutineQuery, InlineRoutineQuery};
#[cfg(test)]
mod tests;

use super::{
    compilation::{RoutineCompilationCatalog, RoutineParserCatalog},
    declaration::RoutineTypeCatalog,
    security::RoutineExecutionAuthority,
    RoutineResolution,
};
use crate::{
    ast::{CreateFunction, FunctionBinding},
    plan::UnifiedPlan,
    semantics::volatility::VolatilityCatalog,
    SQLError, ScalarExpr,
};

/// Metadata and analysis capabilities borrowed for one optimizer invocation.
pub trait RoutinePlanExpressions {
    /// Execute only an immutable call whose inputs the planner proved constant.
    /// The existing runtime boundary retains authorization and session body caches.
    fn evaluate_constant_routine(
        &self,
        expression: &ScalarExpr,
    ) -> Result<uqa_core::Value, SQLError>;

    /// Evaluate a constant call and admit its distinct planning destination before copying the result. Native evaluators also admit their temporary scalar and scope inputs; interpreter rows and results retain their existing execution ownership.
    fn evaluate_constant_routine_with_control(
        &self,
        expression: &ScalarExpr,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<uqa_core::Value>, SQLError> {
        control.check()?;
        let value = self.evaluate_constant_routine(expression)?;
        if control.budget().is_none() {
            Ok(control.finish(value, None)?)
        } else {
            Ok(control.copy_value(&value)?)
        }
    }
}

#[derive(Clone, Copy)]
pub struct RoutineInliningContext<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
    pub catalog: &'a dyn RoutineCompilationCatalog,
    pub relations: &'a dyn crate::binding::stored_relations::StoredRelationCatalog,
    pub authority: &'a dyn RoutineExecutionAuthority,
    pub volatility: &'a dyn VolatilityCatalog,
    pub expressions: &'a dyn RoutinePlanExpressions,
}

impl std::fmt::Debug for RoutineInliningContext<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoutineInliningContext")
            .finish_non_exhaustive()
    }
}

/// A typed replacement, together with the routine identity whose recursive
/// expansion the planner must suppress while simplifying the replacement.
pub struct InlineRoutineExpression {
    pub identity: [u8; 16],
    pub expression: ScalarExpr,
    pub error_context: Option<String>,
}

impl RoutineInliningContext<'_> {
    /// Classify an analyzed/planned expression in its own row scope, including casts and selected operators.
    pub fn expression_volatility(
        &self,
        expression: &ScalarExpr,
        schema: &crate::RowSchema,
    ) -> Result<crate::ast::FunctionVolatility, SQLError> {
        properties::volatility(self, expression, schema)
    }

    pub fn expression_volatility_with_control(
        &self,
        expression: &ScalarExpr,
        schema: &crate::RowSchema,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<crate::ast::FunctionVolatility, SQLError> {
        properties::volatility_with_control(self, expression, schema, control)
    }

    /// Try the selected routine only; a same-named overload cannot replace it.
    /// Argument simplification and lazy branches belong to the caller's planner.
    pub fn prepare(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
    ) -> Result<Option<InlineRoutineExpression>, SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        let replacement = self.prepare_owned(binding, arguments, active, &control)?;
        replacement
            .map(|replacement| {
                Ok(InlineRoutineExpression {
                    identity: replacement.identity,
                    expression: replacement
                        .expression
                        .finish(&control)?
                        .into_uncontrolled()
                        .expect("ordinary inline expression"),
                    error_context: replacement.error_context.map(|frame| {
                        frame
                            .into_uncontrolled()
                            .expect("ordinary inline diagnostic frame")
                    }),
                })
            })
            .transpose()
    }

    /// Expand defaults, named arguments and implicit casts before the planner
    /// simplifies children. Returning an exact invocation prevents a second
    /// overload selection and keeps discarded or repeated arguments typed.
    pub fn materialize_call(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
    ) -> Result<Option<(FunctionBinding, Vec<ScalarExpr>)>, SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        let Some((selected, arguments)) =
            self.materialized_owned_arguments(binding, arguments, &control)?
        else {
            return Ok(None);
        };
        let selected = selected
            .into_uncontrolled()
            .expect("ordinary selected binding");
        let arguments = arguments
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary call arguments");
        let ScalarExpr::Or(arguments) = arguments else {
            unreachable!("routine argument container");
        };
        Ok(Some((selected, arguments)))
    }

    fn selected_function(
        &self,
        binding: &FunctionBinding,
    ) -> Option<std::sync::Arc<super::SQLUserFunction>> {
        super::selected_bound_function(self.routines, binding)
    }

    /// Ordinary messages repeat optimizer-time routine work. Prepared plans
    /// intentionally keep their own custom/generic planning lifetime instead.
    pub fn requires_replanning(&self, plan: &UnifiedPlan) -> bool {
        let mut required = false;
        plan.visit_scalar_expressions(&mut |root| {
            root.visit(&mut |expression| {
                let ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } = expression
                else {
                    return;
                };
                if binding.builtin {
                    return;
                }
                required |= self
                    .routines
                    .lookup_bound_sql_functions_by_binding(binding)
                    .is_some_and(|functions| {
                        functions.iter().any(|function| {
                            function.def.object_id == binding.object_id
                                && super::routine_signature_types(&function.def)
                                    == binding.argument_types
                                && (eligible_definition(&function.def)
                                    || function.def.volatility
                                        == crate::ast::FunctionVolatility::Immutable)
                        })
                    });
            });
        });
        plan.visit_sources(&mut |source| {
            if let crate::plan::SourcePlan::Function {
                binding: Some(binding),
                ..
            } = source
            {
                required |= self.selected_function(binding).is_some_and(|function| {
                    eligible_definition(&function.def)
                        || table::eligible_definition(&function.def)
                        || function.def.volatility == crate::ast::FunctionVolatility::Immutable
                });
            }
        });
        required
    }

    pub fn requires_replanning_with_control(
        &self,
        plan: crate::plan::PlanRef<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let mut required = false;
        let mut status: Result<(), SQLError> = Ok(());
        plan.visit_scalar_expressions(&mut |root| {
            if status.is_err() {
                return;
            }
            status = root.try_visit(&mut |expression| {
                control.check()?;
                if let ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } = expression
                {
                    if !binding.builtin {
                        if let Some(function) = super::selected_bound_function_with_control(
                            self.routines,
                            binding,
                            control,
                        )? {
                            required |= eligible_definition(&function.def)
                                || function.def.volatility
                                    == crate::ast::FunctionVolatility::Immutable;
                        }
                    }
                }
                Ok::<_, SQLError>(true)
            });
        });
        status?;
        let mut status: Result<(), SQLError> = Ok(());
        plan.visit_sources(&mut |source| {
            if status.is_err() {
                return;
            }
            status = (|| {
                control.check()?;
                if let crate::plan::SourcePlan::Function {
                    binding: Some(binding),
                    ..
                } = source
                {
                    if let Some(function) = super::selected_bound_function_with_control(
                        self.routines,
                        binding,
                        control,
                    )? {
                        required |= eligible_definition(&function.def)
                            || table::eligible_definition(&function.def)
                            || function.def.volatility == crate::ast::FunctionVolatility::Immutable;
                    }
                }
                Ok(())
            })();
        });
        status?;
        Ok(required)
    }
}

fn eligible_definition(definition: &CreateFunction) -> bool {
    selection::eligible_definition_with_control(
        definition,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary routine eligibility")
}

fn is_constant(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } | ScalarExpr::RetainedLiteral(_)
    )
}

fn is_null_constant(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(uqa_core::Value::Null)
            | ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Null,
                ..
            }
    )
}
