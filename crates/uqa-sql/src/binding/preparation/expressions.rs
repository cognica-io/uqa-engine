//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar coercion contexts used while preparing a statement.

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
    ) -> Result<Option<ColumnType>, SQLError> {
        if values.iter().all(|value| value.ty.is_none())
            && values.iter().any(ExpressionType::is_deferred)
        {
            return Ok(None);
        }
        let common = crate::type_resolution::select_common_input_type_in_with_control(
            context,
            values
                .iter()
                .filter(|value| !value.is_deferred())
                .map(|value| value.ty.as_ref()),
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?
        .into_uncontrolled()
        .expect("preparation common type has no reservation");
        for value in values {
            self.parameters.coerce_unknown(value, &common)?;
        }
        Ok(Some(common))
    }

    pub(super) fn binary(
        &mut self,
        op: BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
    ) -> Result<ColumnType, SQLError> {
        let [_, _, result] = self.binary_types(op, left, right)?;
        Ok(result)
    }

    pub(super) fn binary_types(
        &mut self,
        op: BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
    ) -> Result<[ColumnType; 3], SQLError> {
        let selected =
            crate::type_resolution::binary_operator_types(op, left.ty.as_ref(), right.ty.as_ref())?;
        self.parameters.coerce_unknown(left, &selected[0])?;
        self.parameters.coerce_unknown(right, &selected[1])?;
        Ok(selected)
    }

    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let value = self.expression_inner(expression, input, subqueries)?;
        self.parameters
            .collations
            .record(expression, value.derivation);
        self.parameters
            .retain_result_collation(expression, &value)?;
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
        if let Some(index) = self.scope.routine_parameter_reference(expression, input) {
            return self.parameters.reference(index);
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
                    self.parameters.retain_call(expression, &selected);
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
                value.ty.as_ref(),
                Some(self.routines),
            )?;
            self.parameters
                .retain_collation(expression, &mut value, oid);
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
            ScalarExpr::Literal(Value::Str(text)) => {
                Ok(ExpressionType::unknown_literal(expression, text.clone()))
            }
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
            ScalarExpr::Row(items) => self.row_expression(items, input, subqueries),
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
        if matches!(result.ty, Some(ColumnType::Record)) {
            result.record_variable =
                crate::type_resolution::field_selection::is_column_reference(expression);
            if let Some(metadata) = input.input_record_metadata(expression) {
                result.record_fields.clone_from(&metadata.types);
                result.record_descriptor.clone_from(&metadata.descriptor);
            } else if let Some(qualifier) =
                crate::type_resolution::field_selection::whole_row_qualifier(expression, input)
            {
                let descriptor = super::fields::whole_row_descriptor(input, qualifier);
                result.record_fields = Some(descriptor.types.clone());
                result.record_descriptor = Some(descriptor);
            } else if result.record_fields.is_none() {
                result.record_fields = self.scope.bind_record_fields(
                    self.routines,
                    expression,
                    input,
                    subqueries,
                    &self.parameters.values(),
                )?;
            }
        }
        Ok(())
    }

    fn row_expression(
        &mut self,
        items: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let mut fields = Vec::new();
        for item in items {
            fields.push(self.expression(item, input, subqueries)?);
        }
        let mut value = ExpressionType::independent(Some(ColumnType::Record), &fields);
        value.record_fields = Some(fields.iter().map(|field| field.ty.clone()).collect());
        value.record_descriptor = Some(std::sync::Arc::new(crate::schema::RecordDescriptor {
            types: value.record_fields.clone().expect("ROW fields"),
            names: None,
            fields: fields
                .iter()
                .map(|field| crate::schema::RecordFieldMetadata {
                    collation_oid: u32::try_from(field.derivation.oid())
                        .expect("ROW field collation OID"),
                    sql_unknown: field.is_sql_unknown(),
                })
                .collect(),
        }));
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
            _ => ExpressionType::resolved(self.known_type(expression, input, subqueries)?),
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
        selected.validate_modifiers(
            name,
            binding,
            args,
            super::super::ordered_calls::Modifiers {
                direct: super::super::ordered_calls::DirectArguments::from(args.as_slice()),
                ordered: order_by.len(),
                within_group,
                distinct: *distinct,
                filtered: filter.is_some(),
            },
            &arguments,
        )?;
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
        let mut selected = selected.overload;
        self.keep_call_selection(expression, &arguments, input_collation, selected.as_mut());
        // Keep the result selected from the observed occurrences: a later sibling can determine a parameter's type after an earlier COLLATE operand was admitted as unknown, so retyping that syntax loses its state.
        let ty = match (result, &selected) {
            (super::function_calls::FunctionResult::Selected(ty), _) => ty,
            (_, Some(selected)) => Some(selected.return_type.clone()),
            (super::function_calls::FunctionResult::Unselected, None) => {
                self.observed_call_type(expression, &arguments, &additional, input, subqueries)?
            }
        };
        if let Some(selected) = &selected {
            self.parameters.retain_call(expression, &selected.binding);
        }
        let selected_binding = selected
            .as_ref()
            .map(|selected| &selected.binding)
            .or(binding);
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
        if !self.routines.is_scalar_function_binding(binding)? {
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
    ) -> Result<Option<FunctionBinding>, SQLError> {
        let mut arguments = self.observe_function_arguments(name, args, &[], input, subqueries)?;
        let mut selected = crate::semantics::resolve_table_function_binding_for_types(
            self.routines,
            name,
            binding,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names: &arguments.names,
                argument_types: &arguments.types(),
                explicit_variadic: arguments.variadic,
            },
        )?;
        let chosen = selected.as_ref().or(binding);
        let signature = self.select_function_arguments(name, chosen, args, &arguments)?;
        self.coerce_function_arguments(name, chosen, args, &mut arguments, &signature)?;
        if let Some(selected) = &mut selected {
            selected.input_collation = arguments.input_collation(args.len(), None, false)?;
        }
        Ok(selected)
    }
}
