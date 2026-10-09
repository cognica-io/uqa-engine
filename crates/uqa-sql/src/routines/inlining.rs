//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-plan expansion of selected SQL routines. Source text is analyzed for
//! each planning attempt; declining expansion never changes the session body cache.

mod arguments;
mod operators;
mod properties;
#[cfg(test)]
mod tests;

use super::{
    body_parameters::sql_body_parameter_scope,
    body_validation::routine_parameter_values,
    compilation::{RoutineCompilationCatalog, RoutineParserCatalog},
    declaration::RoutineTypeCatalog,
    result_check::{sql_function_result_layout, SQLFunctionResultKind},
    security::{ensure_routine_execute_privilege, RoutineExecutionAuthority},
    CompiledFunctionBody, RoutineBody, RoutineResolution,
};
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionBody, FunctionReturns},
    binding::{bind_routine_parameter_references, prepare_routine_body_inputs},
    plan::{ComputePlan, QueryBlockPlan, RelationalPlan, UnifiedPlan},
    semantics::volatility::VolatilityCatalog,
    ColumnType, SQLError, ScalarExpr,
};

/// Metadata and analysis capabilities borrowed for one optimizer invocation.
pub trait RoutinePlanExpressions {
    /// Execute only an immutable call whose inputs the planner proved constant.
    /// The existing runtime boundary retains authorization and session body caches.
    fn evaluate_constant_routine(
        &self,
        expression: &ScalarExpr,
    ) -> Result<uqa_core::Value, SQLError>;
}

#[derive(Clone, Copy)]
pub struct RoutineInliningContext<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
    pub catalog: &'a dyn RoutineCompilationCatalog,
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

    /// Try the selected routine only; a same-named overload cannot replace it.
    /// Argument simplification and lazy branches belong to the caller's planner.
    pub fn prepare(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
    ) -> Result<Option<InlineRoutineExpression>, SQLError> {
        if binding.builtin || binding.resolution_error.is_some() {
            return Ok(None);
        }
        let Some(identity) = binding.object_id else {
            return Ok(None);
        };
        let Some(function) = self.selected_function(binding) else {
            return Ok(None);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(None);
        };
        let specialized = super::invocation::specialized_definition(&function.def, invocation)?;
        let definition = specialized.as_ref().unwrap_or(&function.def);
        let arguments_for_evaluation = arguments;
        let Some(arguments) = arguments::prepare(self, definition, invocation, arguments)? else {
            return Ok(None);
        };
        if let Some(expression) =
            self.constant_call(definition, binding, &arguments, arguments_for_evaluation)?
        {
            return Ok(Some(InlineRoutineExpression {
                identity,
                expression,
            }));
        }
        if !eligible_definition(definition) || active.contains(&identity) {
            return Ok(None);
        }
        match ensure_routine_execute_privilege(self.authority, definition) {
            Ok(()) => {}
            Err(error) if error.sqlstate() == Some("42501") => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some((mut expression, parameters)) = self.analyze_body(&function, definition)? else {
            return Ok(None);
        };
        let properties = properties::inspect(self, &expression, &parameters)?;
        if !properties.permits(definition) {
            return Ok(None);
        }
        let uses = arguments::use_counts(&expression, arguments.len())?;
        for (argument, count) in arguments.iter().zip(uses) {
            if count == 0 && definition.strict {
                return Ok(None);
            }
            if count > 1 && !properties::can_duplicate(self, argument)? {
                return Ok(None);
            }
        }
        arguments::substitute(&mut expression, &arguments);
        Ok(Some(InlineRoutineExpression {
            identity,
            expression,
        }))
    }

    fn constant_call(
        &self,
        definition: &CreateFunction,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        arguments_for_evaluation: &[ScalarExpr],
    ) -> Result<Option<ScalarExpr>, SQLError> {
        if !definition.is_procedure
            && !definition.returns_set()
            && definition.output_params().len() <= 1
            && !super::routine_returns_anonymous_record(definition)
        {
            let layout = super::result_check::declared_sql_function_result(self.types, definition)?;
            if definition.strict && arguments.iter().any(is_null_constant) {
                return Ok(Some(ScalarExpr::TypedLiteral {
                    composite_source: None,
                    value: uqa_core::Value::Null,
                    ty: layout.declared_type.catalog_name(),
                    bound_type: Some(layout.declared_type),
                    parameter_index: None,
                }));
            }
            if definition.volatility == crate::ast::FunctionVolatility::Immutable
                && arguments.iter().all(is_constant)
            {
                let call = ScalarExpr::Func {
                    name: binding.name.clone(),
                    binding: Some(binding.clone()),
                    args: arguments_for_evaluation.to_vec(),
                    distinct: false,
                    order_by: Vec::new(),
                    order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
                    filter: None,
                };
                let value = self.expressions.evaluate_constant_routine(&call)?;
                return Ok(Some(ScalarExpr::TypedLiteral {
                    composite_source: None,
                    value,
                    ty: layout.declared_type.catalog_name(),
                    bound_type: Some(layout.declared_type),
                    parameter_index: None,
                }));
            }
        }
        Ok(None)
    }

    fn analyze_body(
        &self,
        function: &super::SQLUserFunction,
        definition: &CreateFunction,
    ) -> Result<Option<(ScalarExpr, Vec<crate::SQLParam>)>, SQLError> {
        let Some(mut plan) = self.body_plan(function)? else {
            return Ok(None);
        };
        let parameters = routine_parameter_values(self.types, definition);
        let names = matches!(definition.body, FunctionBody::Source(_))
            .then(|| sql_body_parameter_scope(definition, &parameters))
            .transpose()?;
        let snapshot = self.catalog.binding_snapshot()?;
        let result = prepare_routine_body_inputs(
            self.routines,
            &mut plan,
            &parameters,
            &snapshot.context(),
            self.catalog,
            names.as_ref(),
        )?;
        if let Some(names) = &names {
            bind_routine_parameter_references(
                self.routines,
                &mut plan,
                &parameters,
                &snapshot.context(),
                names,
            )?;
        }
        let UnifiedPlan::Query(query) = plan else {
            return Ok(None);
        };
        if !query.ctes.is_empty() {
            return Ok(None);
        }
        let RelationalPlan::QueryBlock(mut block) = query.root else {
            return Ok(None);
        };
        if !simple_select(&block) {
            return Ok(None);
        }
        let layout = sql_function_result_layout(self.types, definition, Some(&result))?;
        if layout.kind == SQLFunctionResultKind::Tuple {
            return Ok(None);
        }
        let mut expression = block.projections.remove(0).expr;
        let actual = result
            .column_types()
            .and_then(|types| types.first())
            .and_then(Option::as_ref);
        if layout.kind == SQLFunctionResultKind::Void && actual != Some(&ColumnType::Void) {
            return Ok(None);
        }
        if actual != Some(&layout.declared_type) {
            expression = ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(expression),
                ty: layout.declared_type.catalog_name(),
            };
        }
        Ok(Some((expression, parameters)))
    }

    /// Expand defaults, named arguments and implicit casts before the planner
    /// simplifies children. Returning an exact invocation prevents a second
    /// overload selection and keeps discarded or repeated arguments typed.
    pub fn materialize_call(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
    ) -> Result<Option<(FunctionBinding, Vec<ScalarExpr>)>, SQLError> {
        let Some(function) = self.selected_function(binding) else {
            return Ok(None);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(None);
        };
        let specialized = super::invocation::specialized_definition(&function.def, invocation)?;
        let definition = specialized.as_ref().unwrap_or(&function.def);
        let Some(mut arguments) = arguments::prepare(self, definition, invocation, arguments)?
        else {
            return Ok(None);
        };
        let mut selected = binding.clone();
        let invocation = selected.invocation.as_mut().expect("selected invocation");
        invocation.argument_positions = definition
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                super::body_parameters::is_sql_body_parameter(parameter).then_some(index)
            })
            .collect();
        invocation.argument_targets = invocation
            .argument_positions
            .iter()
            .map(|index| invocation.parameter_types[*index].clone())
            .collect();
        invocation.argument_sources = invocation
            .argument_targets
            .iter()
            .cloned()
            .map(Some)
            .collect();
        if let crate::ast::RoutineVariadicMode::Expanded { parameter_index } =
            invocation.variadic_mode
        {
            invocation.variadic_mode =
                crate::ast::RoutineVariadicMode::Explicit { parameter_index };
        }
        if let crate::ast::RoutineVariadicMode::Explicit { parameter_index } =
            invocation.variadic_mode
        {
            let position = invocation
                .argument_positions
                .iter()
                .position(|index| *index == parameter_index)
                .ok_or_else(|| SQLError::Internal("variadic call lost its argument".into()))?;
            let value = std::mem::replace(
                &mut arguments[position],
                ScalarExpr::Literal(uqa_core::Value::Null),
            );
            let marker =
                FunctionBinding::dispatched(crate::ast::FunctionDispatch::VariadicArgument);
            arguments[position] = ScalarExpr::Func {
                name: marker.name.clone(),
                binding: Some(marker),
                args: vec![value],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
                order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
            };
        }
        Ok(Some((selected, arguments)))
    }

    fn selected_function(
        &self,
        binding: &FunctionBinding,
    ) -> Option<std::sync::Arc<super::SQLUserFunction>> {
        if binding.builtin || binding.resolution_error.is_some() {
            return None;
        }
        let identity = binding.object_id?;
        self.routines
            .lookup_bound_sql_functions_by_binding(binding)?
            .into_iter()
            .find(|function| {
                !function.def.is_procedure
                    && function.def.object_id == Some(identity)
                    && super::routine_signature_types(&function.def) == binding.argument_types
            })
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
        required
    }

    fn body_plan(
        &self,
        function: &super::SQLUserFunction,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        match &function.body {
            RoutineBody::Bound(body) => match body.as_ref() {
                CompiledFunctionBody::SQL(plans) if plans.len() == 1 => Ok(Some(plans[0].clone())),
                _ => Ok(None),
            },
            RoutineBody::Source => {
                let FunctionBody::Source(source) = &function.def.body else {
                    return Err(SQLError::Internal(
                        "source routine has no SQL source".into(),
                    ));
                };
                // Parse the complete message once, then decline a multi-statement
                // body before semantic compilation. Lexical notices still belong
                // to every attempted expansion, as in inline_function.
                super::compilation::with_parser_context(self.parsers, || {
                    let statements = crate::parse_statements(source)?;
                    let [statement] = statements.as_slice() else {
                        return Ok(None);
                    };
                    Ok(Some(UnifiedPlan::lower_with(
                        statement.compile()?,
                        &|name: &str| self.catalog.has_registered_aggregate_function(name),
                    )))
                })
            }
        }
    }
}

fn eligible_definition(definition: &CreateFunction) -> bool {
    definition.language == "sql"
        && !definition.is_procedure
        && !definition.security.security_definer
        && !definition.returns_set()
        && definition.config.is_empty()
        && definition.output_params().len() <= 1
        && !matches!(&definition.returns, FunctionReturns::Scalar { type_name }
            if crate::type_resolution::canonical_routine_type_name(type_name) == "record")
}

fn simple_select(block: &QueryBlockPlan) -> bool {
    matches!(block.compute, ComputePlan::Project)
        && block.from.is_none()
        && block.r#where.is_none()
        && block.group_by.is_empty()
        && block.grouping_sets.is_empty()
        && block.having.is_none()
        && block.windows.is_empty()
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.limit.is_none()
        && block.offset.is_none()
        && block.locking.is_empty()
        && block.subqueries.is_empty()
        && block.projections.len() == 1
}

fn is_constant(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
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
