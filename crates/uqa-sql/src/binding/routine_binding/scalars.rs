//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select stored scalar calls in their lexical row and query scope.

use super::{
    FunctionBinding, FunctionTypeResolver, QueryPlan, RoutineResolution, RowSchema, SQLError,
    SQLParam, ScalarExpr, SchemaScope,
};

impl SchemaScope {
    pub(in crate::binding) fn bind_scalar_routines_for_storage(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        crate::catalog::work::inline(self.bind_scalar_routines_for_storage_with_work(
            engine,
            expression,
            schema,
            subqueries,
            params,
            &mut (),
        ))
    }

    pub(in crate::binding) async fn bind_scalar_routines_for_storage_with_work<
        W: crate::catalog::work::Work + Send,
    >(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        work: &mut W,
    ) -> Result<(), SQLError> {
        let owner = self.production.clone();
        let control = owner.control();
        work.checkpoint().await?;
        control.check()?;
        let schema = self.with_stored_outer_internal_aliases(schema)?;
        let schema = &schema;
        self.canonicalize_stored_outer_columns(expression, schema);
        self.expand_stored_whole_rows(expression, schema);
        self.canonicalize_routine_parameters(expression, schema);
        self.resolve_variable_sites(expression, schema);
        match self.scalar_binding {
            crate::binding::ScalarBindingMode::References
            | crate::binding::ScalarBindingMode::RoutineParameters => return Ok(()),
            crate::binding::ScalarBindingMode::CompositeInputs => {
                return self.retain_composite_inputs_in_scope(
                    engine, expression, schema, subqueries, params,
                )
            }
            crate::binding::ScalarBindingMode::Stored => {}
        }
        let mut selection = Dispatch {
            scope: self,
            engine,
            schema,
            subqueries,
            params,
        };
        if W::MAY_SUSPEND {
            let mut work = crate::catalog::work::ReferenceWork {
                control: &control,
                work,
            };
            crate::plan::edit_statement_scalar(expression, &control, &mut work, &mut selection)
                .await?;
        } else {
            let mut result = Ok(());
            crate::plan::rewrite_scalar_expression(expression, &mut |node| {
                if result.is_ok() {
                    result = crate::catalog::work::inline(selection.node(node, &control, work));
                }
            });
            result?;
        }
        let comparison_inputs = crate::ir::observation::visit_in(
            expression, &control, work, crate::ir::children::Scope::Complete,
            &mut |node| Ok(if matches!(node, ScalarExpr::Func { binding: Some(binding), .. }
                if matches!(binding.dispatch, Some(crate::ast::FunctionDispatch::IsDistinct | crate::ast::FunctionDispatch::ValueNullTest { .. }))) {
                crate::ir::observation::Decision::Stop
            } else { crate::ir::observation::Decision::Descend }),
        ).await?;
        if comparison_inputs {
            self.retain_composite_inputs_in_scope(engine, expression, schema, subqueries, params)?;
        }
        self.bind_stored_scalar_types(engine, expression, schema, subqueries, params)?;
        self.record_composite_dependencies(engine, expression, schema, subqueries, params)
    }

    /// Name user-defined types by OID identity and keep the enum constants that binding coerces from `unknown` literals by label identity, as `PostgreSQL` stores type and label OIDs in analyzed expressions.
    fn bind_stored_scalar_types(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        crate::binding::stored_types::bind_scalar_type_identities(expression, &mut |name| {
            engine.resolve_type_name(name)
        })?;
        let resolver = self.query_function_type_resolver_for_subqueries(
            engine,
            std::slice::from_ref(expression),
            schema,
            subqueries,
            params,
        )?;
        // The coercions binding adds to an operator's operands, the constants it reads from `unknown` literals and the relabels of `oid` alias operands, are stored, as `PostgreSQL` stores them in an analyzed expression.
        if self.preserve_syntax_shape {
            crate::type_resolution::store_syntax_operand_coercions(
                expression, schema, params, &resolver,
            )?;
        } else {
            crate::type_resolution::store_operand_coercions(expression, schema, params, &resolver)?;
        }
        if !crate::type_resolution::contains_unknown_literal(expression) {
            return Ok(());
        }
        crate::type_resolution::fold_stored_enum_constants(expression, schema, params, &resolver)
            .map(drop)
    }
}

use crate::{
    catalog::work::{ReferenceWork, Work},
    plan::retirement::Pending,
};
use uqa_core::memory::ProductionControl;

struct Dispatch<'a> {
    scope: &'a mut SchemaScope,
    engine: &'a dyn RoutineResolution,
    schema: &'a RowSchema,
    subqueries: &'a [QueryPlan],
    params: &'a [SQLParam],
}

impl crate::plan::ScalarEditIdentity<Pending<ScalarExpr>> for Dispatch<'_> {
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}
}

impl<W: Work + Send> crate::plan::ScalarEdit<ReferenceWork<'_, '_, W>, Pending<ScalarExpr>>
    for Dispatch<'_>
{
    async fn apply(
        &mut self,
        (): (),
        mut expression: Pending<ScalarExpr>,
        control: &ProductionControl<'_>,
        work: &mut ReferenceWork<'_, '_, W>,
    ) -> Result<Pending<ScalarExpr>, SQLError> {
        self.node(&mut expression, control, work.work).await?;
        Ok(expression)
    }
}

impl Dispatch<'_> {
    async fn node<W: Work + Send>(
        &mut self,
        expression: &mut ScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if matches!(expression, ScalarExpr::Func { order_syntax, name, binding, order_by, .. }
            if crate::binding::ordered_calls::uses_ordered_arguments(*order_syntax, name, binding.as_ref(), order_by.len())
                || crate::binding::ordered_calls::is_ordered_set(name))
        {
            return self.scope.bind_ordered_function_for_storage(
                self.engine,
                expression,
                self.schema,
                self.subqueries,
                self.params,
            );
        }
        let ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        else {
            return Ok(());
        };
        let dispatch = binding.as_ref().and_then(|binding| binding.dispatch);
        if dispatch.is_some()
            && !matches!(
                dispatch,
                Some(crate::ast::FunctionDispatch::NumericOperator(_))
            )
        {
            return Ok(());
        }
        let resolver = self.scope.query_function_type_resolver_for_subqueries(
            self.engine,
            args,
            self.schema,
            self.subqueries,
            self.params,
        )?;
        let arguments = crate::type_resolution::signature_with_work(
            args,
            self.params,
            control,
            work,
            |value| {
                crate::type_resolution::common_context_expression_type_with_control(
                    value,
                    self.schema,
                    self.params,
                    Some(&resolver),
                    control,
                )
            },
        )
        .await?;
        let (names, types, variadic) = &*arguments;
        if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) = dispatch {
            let selected = crate::type_resolution::numeric_operator_types(operator, types)?;
            binding
                .as_mut()
                .expect("structural operator")
                .argument_types = selected
                .arguments
                .iter()
                .map(crate::ColumnType::sql_name)
                .collect();
            return Ok(());
        }
        let selected = if crate::is_fixed_builtin(name) {
            crate::type_resolution::resolve_fixed_builtin_call_with_control(
                name,
                binding.as_ref(),
                names,
                types,
                *variadic,
                Some(&resolver),
                control,
            )?
            .map(|resolved| resolved.selected)
        } else if crate::catalog::json_path::JsonPathFunction::lookup(name).is_some() {
            crate::type_resolution::json_path::resolve_call(
                name,
                binding.as_ref(),
                crate::type_resolution::RoutineCallDescriptor {
                    argument_names: names,
                    argument_types: types,
                    explicit_variadic: *variadic,
                },
                Some(&resolver),
                control,
            )?
        } else if let Some(array) =
            crate::type_resolution::resolve_array_transform_call_with_control(
                name,
                binding.as_ref(),
                crate::type_resolution::ArrayTransformArguments {
                    expressions: args,
                    types,
                    parameters: self.params,
                    explicit_variadic: *variadic,
                },
                &resolver,
                control,
            )?
        {
            array.overload
        } else {
            resolver.resolve_function_overload_with_control(
                name,
                binding.as_ref(),
                names,
                types,
                *variadic,
                control,
            )?
        };
        if let Some(selected) = selected {
            if !self.scope.stored_call_origin.keeps_runtime_lookup(
                name,
                binding.as_ref(),
                &selected.binding,
            ) {
                let (binding_value, lease) = if control.budget().is_some() {
                    selected
                        .binding
                        .clone_with_input_work(control, &mut ReferenceWork { control, work })
                        .await?
                        .into_parts()
                } else {
                    (
                        selected
                            .into_uncontrolled()
                            .expect("ordinary routine overload")
                            .binding,
                        None,
                    )
                };
                FunctionBinding::rebind(binding, binding_value);
                self.scope.stored_binding_memory =
                    control.combine(self.scope.stored_binding_memory.take(), lease);
            }
        }
        if !self.scope.preserve_syntax_shape {
            crate::type_resolution::json_path::normalize_retained(expression)?;
        }
        Ok(())
    }
}
