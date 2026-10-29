//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar coercion contexts used while preparing a statement.

use super::function_calls::FunctionResult;
use super::ownership::AnalysisType;
use super::{
    error, ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr,
};
use crate::ast::{BinaryOp, FunctionBinding};
use crate::catalog::collations::derivation::Derivation;
use uqa_core::Value;

mod nodes;

impl Preparation<'_> {
    /// The common type of the inputs of the construct `context`, as `select_common_type` chooses it, with the unknown inputs coerced to it.
    pub(super) fn common(
        &mut self,
        context: crate::type_resolution::CommonTypeContext,
        values: &mut [ExpressionType],
    ) -> Result<Option<AnalysisType>, SQLError> {
        if values.iter().all(|value| value.ty.is_none())
            && values.iter().any(ExpressionType::is_deferred)
        {
            return Ok(None);
        }
        let control_owner = self.scope.control().capture();
        let control = control_owner.control();
        let common = crate::type_resolution::select_common_input_type_in_with_control(
            context,
            values
                .iter()
                .filter(|value| !value.is_deferred())
                .map(|value| value.ty.as_deref()),
            &control,
        )?;
        for value in values {
            self.parameters.coerce_unknown(value, &common)?;
        }
        Ok(Some(AnalysisType::from_produced(common, &control)?))
    }

    pub(super) fn binary(
        &mut self,
        op: BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
    ) -> Result<AnalysisType, SQLError> {
        let [_, _, result] = self.binary_types(op, left, right)?;
        Ok(result)
    }

    pub(super) fn binary_types(
        &mut self,
        op: BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
    ) -> Result<[AnalysisType; 3], SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let selected = crate::type_resolution::binary_operator_types_with_control(
            op,
            left.ty.as_deref(),
            right.ty.as_deref(),
            &control,
        )?;
        self.parameters.coerce_unknown(left, &selected[0])?;
        self.parameters.coerce_unknown(right, &selected[1])?;
        Ok([
            AnalysisType::from_produced(selected[0].clone_with_control(&control)?, &control)?,
            AnalysisType::from_produced(selected[1].clone_with_control(&control)?, &control)?,
            AnalysisType::from_produced(selected[2].clone_with_control(&control)?, &control)?,
        ])
    }

    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        if let Some(outer) = self.scope.stored_expression_outer.as_ref() {
            let identity = match expression {
                ScalarExpr::Column(column) => Some((None, column.as_str())),
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    Some((Some(qualifier.as_str()), column.as_str()))
                }
                _ => None,
            };
            if let (Some((qualifier, column)), Some(start)) = (
                identity,
                input.physical_width().checked_sub(outer.physical_width()),
            ) {
                let identity = crate::ColumnIdentity::from_parts_with_control(
                    qualifier,
                    column,
                    &self.scope.control(),
                )?;
                if let Some(slot) = input
                    .physical_slot_for_identity(&identity)
                    .filter(|slot| *slot >= start)
                {
                    if let Some(column) = outer.unique_internal_column_for_slot(slot - start) {
                        self.parameters
                            .retain_stored_reference(expression, column)?;
                    }
                }
            }
        }
        let value = self.expression_inner(expression, input, subqueries)?;
        self.parameters
            .collations
            .record(expression, value.derivation)?;
        self.parameters
            .retain_result_metadata_for(expression, &value)?;
        if self.parameters.retains_composite_inputs()
            && crate::binding::composite_inputs::owned::selected(expression)
        {
            let control_owner = self.scope.control().capture();
            let control = control_owner.control();
            let parameters = self.parameters.values()?;
            let replacement = self.scope.retain_composite_inputs_in_scope_owned(
                self.routines,
                expression.copy_for_rewrite(&control)?,
                input,
                subqueries,
                &parameters,
            )?;
            self.parameters
                .retain_composite_input(expression, replacement)?;
        }
        Ok(value)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered dispatch covers every scalar expression"
    )]
    fn expression_inner(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        if let Some(sites) = self.scope.variable_sites.as_mut() {
            if let Some(site) = sites.site(expression) {
                if super::super::variable_sites::takes(input, &sites.names[site]) {
                    sites.resolutions.as_mut_slice()[site] =
                        super::super::VariableSiteResolution::Column;
                    let owner = self.scope.control().capture();
                    let control = owner.control();
                    let name = self
                        .scope
                        .variable_sites
                        .as_ref()
                        .expect("variable site scope")
                        .names[site]
                        .copy_for_rewrite(&control)?;
                    return self.expression_inner(&name, input, subqueries);
                }
            }
        }
        if let Some(index) = self.scope.routine_parameter_reference(expression, input) {
            return self.parameters.named_reference(expression, index);
        }
        self.check_schema_subquery(expression)?;
        if let ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } = expression
        {
            if binding.builtin
                && binding.dispatch == Some(crate::ast::FunctionDispatch::MergeAction)
                && !self.scope.merge_returning
                && !self.parameters.is_output_binding()
            {
                return Err(error(
                    "42601",
                    "MERGE_ACTION() can only be used in the RETURNING list of a MERGE command"
                        .into(),
                ));
            }
            if let Some(symbol) = binding
                .dispatch
                .and_then(|dispatch| dispatch.json_extraction_symbol())
            {
                return self.json_extraction(symbol, args, input, subqueries);
            }
            if let Some(access) = binding.dispatch.and_then(|dispatch| {
                crate::ast::ArrayAccess::from_dispatch(dispatch, args.len(), |index| {
                    matches!(args[index], ScalarExpr::Literal(Value::Null))
                })
            }) {
                let access = access?;
                if binding.dispatch == Some(crate::ast::FunctionDispatch::ArraySlices) {
                    let mut selected = binding.clone();
                    selected.dispatch = Some(crate::ast::FunctionDispatch::ArraySlicesWithBounds(
                        access.slice_bounds().expect("slice bounds"),
                    ));
                    self.parameters.retain_call(expression, &selected)?;
                }
                return self.array_access(access, args, input, subqueries);
            }
            if let Some(crate::ast::FunctionDispatch::BooleanTest(test)) = binding.dispatch {
                let [argument] = args.as_slice() else {
                    return Err(SQLError::Internal("Boolean test takes one input".into()));
                };
                let value = self.boolean_expression(argument, input, subqueries, test.label())?;
                return Ok(ExpressionType::independent(
                    Some(ColumnType::Boolean),
                    &[value],
                ));
            }
        }
        if let Some(clause) = crate::catalog::collations::expression::scalar_clause(expression)? {
            let mut value = self.expression(clause.argument, input, subqueries)?;
            let oid = crate::catalog::collations::expression::select(
                clause.name,
                clause.oid,
                value.ty.as_deref(),
                Some(self.routines),
            )?;
            self.parameters
                .retain_collation(expression, &mut value, oid)?;
            return Ok(value);
        }
        if self.schema_expression.is_some() {
            if let Some(qualifier) = expression.reference_qualifier() {
                if !input.has_qualifier(qualifier) {
                    return Err(error(
                        "42P01",
                        format!("missing FROM-clause entry for table \"{qualifier}\""),
                    ));
                }
            }
        }
        self.check_schema_column(expression)?;
        match expression {
            ScalarExpr::ScalarSubquery(index) => {
                self.scalar_subquery_expression(*index, input, subqueries)
            }
            ScalarExpr::Param(index) => self.parameters.reference(*index),
            ScalarExpr::Literal(Value::Null) => Ok(ExpressionType::unknown()),
            ScalarExpr::Literal(Value::Str(text)) => Ok(ExpressionType::unknown_literal_produced(
                expression,
                self.scope.control().copy_text(text)?,
            )),
            ScalarExpr::Collated {
                expr,
                ty,
                collation,
            } => self.collated_expression(expr, ty, *collation, input, subqueries),
            ScalarExpr::Cast { expr, ty, .. } => self.cast_expression(expr, ty, input, subqueries),
            ScalarExpr::Binary { op, lhs, rhs } => {
                self.binary_expression(*op, lhs, rhs, input, subqueries)
            }
            ScalarExpr::Not(inner) => self.not_expression(inner, input, subqueries),
            ScalarExpr::And(items) => {
                self.boolean_chain_expression(items, "AND", input, subqueries)
            }
            ScalarExpr::Or(items) => self.boolean_chain_expression(items, "OR", input, subqueries),
            ScalarExpr::IsNull { expr, .. } => self.null_test_expression(expr, input, subqueries),
            ScalarExpr::UnaryMinus(inner) => {
                self.unary_minus_expression(expression, inner, input, subqueries)
            }
            ScalarExpr::Array(items) => self.array_expression(items, input, subqueries),
            ScalarExpr::CompositeRow { items, binding, .. } => {
                self.composite_row_expression(items, binding, input, subqueries)
            }
            ScalarExpr::Row(items) => self.row_expression(expression, items, input, subqueries),
            ScalarExpr::Func { .. } => {
                let mut result = self.function_expression(expression, input, subqueries)?;
                self.expression_record_fields(&mut result, expression, input, subqueries)?;
                Ok(result)
            }
            ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. }
            | ScalarExpr::WindowCall { .. } => {
                let mut result = self.compound_expression(expression, input, subqueries)?;
                self.expression_record_fields(&mut result, expression, input, subqueries)?;
                Ok(result)
            }
            ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::RetainedLiteral(_)
            | ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default => self.leaf_expression(expression, input, subqueries),
        }
    }

    fn expression_record_fields(
        &mut self,
        result: &mut ExpressionType,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        if matches!(result.ty.as_deref(), Some(ColumnType::Record)) {
            result.record_variable =
                crate::type_resolution::field_selection::is_column_reference(expression);
            if let Some(metadata) = input.input_record_metadata(expression) {
                result.record_fields.clone_from(&metadata.types);
                result.record_descriptor.clone_from(&metadata.descriptor);
            } else if let Some(qualifier) =
                crate::type_resolution::field_selection::whole_row_qualifier(expression, input)
            {
                let descriptor =
                    super::fields::whole_row_descriptor(input, qualifier, &self.scope.control())?;
                result.record_fields = Some(descriptor.types.clone());
                result.record_descriptor = Some(descriptor);
            } else if result.record_fields.is_none() {
                result.record_fields = self.scope.bind_record_fields(
                    self.routines,
                    expression,
                    input,
                    subqueries,
                    &self.parameters.values()?,
                )?;
            }
        }
        Ok(())
    }

    fn row_expression(
        &mut self,
        expression: &ScalarExpr,
        items: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let control_owner = self.scope.control().capture();
        let control = control_owner.control();
        let mut fields = super::ownership::AnalysisVec::new(&control);
        let mut expanded = self.parameters.take_row_expansion(expression);
        let mut expansion = 0;
        for (position, item) in items.iter().enumerate() {
            if matches!(item, ScalarExpr::QualifiedStar(_)) {
                if expansion == expanded.len() {
                    let columns =
                        crate::semantics::row_star_fields_with_control(item, input, &control)?
                            .expect("qualified row star");
                    expanded.push((position, columns), &control)?;
                }
                for column in expanded[expansion].1.iter() {
                    fields.push(self.expression(column, input, subqueries)?, &control)?;
                }
                expansion += 1;
            } else {
                fields.push(self.expression(item, input, subqueries)?, &control)?;
            }
        }
        self.parameters.retain_row_expansion(expression, expanded)?;
        let mut value = ExpressionType::independent(Some(ColumnType::Record), &fields);
        let record_types = crate::schema::RecordFields::from_types_with_control(
            fields.iter().map(|field| field.ty.as_deref()),
            &control,
        )?;
        let mut metadata = uqa_core::memory::ProductionVec::new(control);
        metadata.reserve(fields.len())?;
        for field in &fields {
            metadata.push_copy(crate::schema::RecordFieldMetadata {
                collation_oid: u32::try_from(field.derivation.oid())
                    .expect("ROW field collation OID"),
                sql_unknown: field.is_sql_unknown(),
            })?;
        }
        value.record_descriptor = Some(crate::schema::RecordDescriptor::from_parts_with_control(
            record_types.clone(),
            None,
            metadata.finish()?,
            &control,
        )?);
        value.record_fields = Some(record_types);
        value.row_constructor = Some(fields);
        Ok(value)
    }

    fn compound_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        Ok(match expression {
            ScalarExpr::Between { expr, low, high } => {
                let mut value = self.expression(expr, input, subqueries)?;
                let mut low = self.expression(low, input, subqueries)?;
                let mut high = self.expression(high, input, subqueries)?;
                self.binary(BinaryOp::GreaterEqual, &mut value, &mut low)?;
                let first = value.derivation.combine(low.derivation).error_only();
                self.binary(BinaryOp::LessEqual, &mut value, &mut high)?;
                let mut result = ExpressionType::resolved(Some(ColumnType::Boolean));
                result.derivation =
                    first.combine(value.derivation.combine(high.derivation).error_only());
                result
            }
            ScalarExpr::InList { .. } => self.in_list(expression, input, subqueries)?,
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => self.case_expression(
                base.as_deref(),
                when,
                else_branch.as_deref(),
                input,
                subqueries,
            )?,
            ScalarExpr::Exists {
                subquery: index, ..
            } => {
                self.scalar_query_schema(*index, input, subqueries)?;
                ExpressionType::resolved(Some(ColumnType::Boolean))
            }
            ScalarExpr::InSubquery { expr, subquery, .. } => {
                let calls = self.set_calls();
                let mut value = self.expression(expr, input, subqueries)?;
                let schema = self.scalar_query_schema(*subquery, input, subqueries)?;
                let mut result = ExpressionType::resolved(schema.column_type(0).cloned());
                if let Some(oid) = schema.column_collation(0) {
                    result.derivation = Derivation::implicit(i64::from(oid));
                }
                self.binary(BinaryOp::Equal, &mut value, &mut result)?;
                self.reject_set_inputs(
                    calls,
                    crate::semantics::sets::placement::row_comparison_set_error,
                )?;
                ExpressionType::combined(Some(ColumnType::Boolean), &[value, result])
            }
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                let calls = self.set_calls();
                let mut arguments =
                    self.observe_function_arguments(name, args, &[], input, subqueries)?;
                let selected = self.select_function_arguments(name, None, args, &arguments)?;
                self.coerce_function_arguments(name, None, args, &mut arguments, &selected)?;
                let additional =
                    self.function_modifier_collations(&[], filter.as_deref(), input, subqueries)?;
                self.reject_set_inputs(calls, || {
                    crate::semantics::sets::placement::conditional_set_error(
                        "window function calls cannot contain set-returning function calls",
                    )
                })?;
                self.check_schema_window(name, (args, filter.is_some(), *modifiers), input)?;
                if self.schema_expression.is_some() {
                    self.window_specification(spec, input, subqueries)?;
                }
                super::function_calls::collated_result(
                    self.observed_call_type(
                        expression,
                        &arguments,
                        &additional,
                        input,
                        subqueries,
                    )?,
                    &arguments.values,
                    None,
                    additional.diagnostics,
                )
            }
            _ => ExpressionType::resolved_owned(self.known_type(expression, input, subqueries)?),
        })
    }

    fn function_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let ScalarExpr::Func {
            order_syntax,
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } = expression
        else {
            unreachable!("function expression");
        };
        let binding = binding.as_ref();
        let calls = self.set_calls();
        let within_group = super::super::ordered_calls::uses_ordered_arguments(
            *order_syntax,
            name,
            binding,
            order_by.len(),
        );
        let ordered: &[crate::ScalarOrder] = if within_group { order_by } else { &[] };
        let mut arguments =
            self.observe_function_arguments(name, args, ordered, input, subqueries)?;
        let additional = self.function_modifier_collations(
            if within_group { &[] } else { order_by },
            filter.as_deref(),
            input,
            subqueries,
        )?;
        if let Some(mut field) = self.declared_field_selection(binding, args, &arguments, input)? {
            self.check_schema_function(expression, binding, input)?;
            field.derivation = field.derivation.combine(additional.diagnostics);
            return Ok(field);
        }
        let selected = if within_group || super::super::ordered_calls::is_ordered_set(name) {
            self.select_ordered_function_arguments(name, binding, args, ordered, &arguments)?
        } else {
            self.select_function_arguments(name, binding, args, &arguments)?
        };
        selected.validate_call_modifiers(expression, within_group, &arguments)?;
        // Pure name/signature validation must finish before implicit input
        // conversion: a rejected scalar call cannot run a domain CHECK.
        if within_group && selected.overload.is_none() {
            self.known_type(expression, input, subqueries)?;
        }
        if let Some(selected) = selected
            .overload
            .as_ref()
            .filter(|selected| !selected.binding.builtin)
        {
            self.check_selected_scalar_modifiers(
                name,
                &selected.binding,
                *distinct,
                order_by,
                filter.is_some(),
            )?;
        }
        let result =
            self.coerce_function_arguments(name, binding, args, &mut arguments, &selected)?;
        let input_collation = arguments.input_collation(args.len(), selected.kind, within_group)?;
        let selected =
            self.keep_call_selection(expression, &arguments, input_collation, selected.overload)?;
        // Keep the result selected from the observed occurrences: a later sibling can determine a parameter's type after an earlier COLLATE operand was admitted as unknown, so retyping that syntax loses its state.
        let (ty, dispatched) = match (result, &selected) {
            (FunctionResult::Bound(ty, binding), _) => (Some(ty), Some(binding)),
            (FunctionResult::Selected(ty), _) => (ty, None),
            (_, Some(selected)) => (
                Some(super::ownership::AnalysisType::from_produced(
                    selected
                        .return_type
                        .clone_with_control(&self.scope.control())?,
                    &self.scope.control(),
                )?),
                None,
            ),
            (FunctionResult::Unselected, None) => (
                self.observed_call_type(expression, &arguments, &additional, input, subqueries)?,
                None,
            ),
        };
        let retained_binding = selected
            .as_ref()
            .map(|selected| &selected.binding)
            .or(dispatched.as_deref());
        if let Some(retained) = retained_binding {
            self.parameters.retain_call(expression, retained)?;
        }
        let selected_binding = retained_binding.or(binding);
        self.check_schema_function(expression, selected_binding, input)?;
        self.check_set_inputs(name, selected_binding, calls)?;
        self.place_set_call(name, selected_binding, calls)?;
        Ok(arguments.into_collated_result(
            ty,
            binding,
            additional.diagnostics,
            *distinct,
            within_group.then_some(args.len()),
        ))
    }

    fn check_selected_scalar_modifiers(
        &self,
        name: &str,
        binding: &FunctionBinding,
        distinct: bool,
        order_by: &[crate::ScalarOrder],
        filtered: bool,
    ) -> Result<(), SQLError> {
        if !self
            .routines
            .is_scalar_function_binding_with_control(binding, &self.scope.control())?
        {
            return Ok(());
        }
        let modifier = if distinct {
            Some("DISTINCT")
        } else if !order_by.is_empty() {
            Some("ORDER BY")
        } else if filtered {
            Some("FILTER")
        } else {
            None
        };
        modifier.map_or(Ok(()), |modifier| {
            Err(error(
                "42809",
                format!("{modifier} specified, but {name} is not an aggregate function"),
            ))
        })
    }

    pub(super) fn call(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<uqa_core::memory::Produced<FunctionBinding>>, SQLError> {
        let mut arguments = self.observe_function_arguments(name, args, &[], input, subqueries)?;
        let types = arguments.types()?;
        if binding.is_none()
            && crate::type_resolution::function_arguments_have_runtime_types(args, &types)?
        {
            return Ok(None);
        }
        let selected = crate::semantics::resolve_table_function_binding_for_types_with_control(
            self.routines,
            name,
            binding,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names: &arguments.names,
                argument_types: &types,
                explicit_variadic: arguments.variadic,
            },
            &self.scope.control(),
        )?;
        let chosen = selected.as_deref().or(binding);
        let signature = self.select_function_arguments(name, chosen, args, &arguments)?;
        self.coerce_function_arguments(name, chosen, args, &mut arguments, &signature)?;
        let collation = arguments.input_collation(args.len(), None, false)?;
        selected
            .map(|selected| {
                let (mut selected, memory) = selected.into_parts();
                selected.input_collation = collation;
                self.scope
                    .control()
                    .finish(selected, memory)
                    .map_err(Into::into)
            })
            .transpose()
    }
}
