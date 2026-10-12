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
            if let ScalarExpr::QualifiedColumn { qualifier, .. }
            | ScalarExpr::QualifiedStar(qualifier) = expression
            {
                if !input.has_qualifier(qualifier) {
                    return Err(error(
                        "42P01",
                        format!("missing FROM-clause entry for table \"{qualifier}\""),
                    ));
                }
            }
        }
        let ty = match expression {
            ScalarExpr::ScalarSubquery(index) => {
                let query = subqueries.get(*index).ok_or_else(|| {
                    error("XX000", "scalar subquery slot is outside its plan".into())
                })?;
                let schema = self.query(query, Some(input))?;
                let mut result = ExpressionType::resolved(schema.column_type(0).cloned());
                if let Some(oid) = schema.column_collation(0) {
                    result.derivation = Derivation::implicit(i64::from(oid));
                }
                result.record_fields = schema.record_fields(0).cloned();
                return Ok(result);
            }
            ScalarExpr::Param(index) => return self.parameters.reference(*index),
            ScalarExpr::Literal(Value::Null) => return Ok(ExpressionType::unknown()),
            ScalarExpr::Literal(Value::Str(text)) => {
                return Ok(ExpressionType::unknown_literal(expression, text.clone()));
            }
            ScalarExpr::Cast { expr, ty, .. } => {
                return self.cast_expression(expr, ty, input, subqueries);
            }
            ScalarExpr::Binary { op, lhs, rhs } => {
                let mut left = self.expression(lhs, input, subqueries)?;
                let mut right = self.expression(rhs, input, subqueries)?;
                if matches!(
                    (lhs.as_ref(), rhs.as_ref()),
                    (ScalarExpr::Row(_), ScalarExpr::Row(_))
                ) {
                    self.coerce_row_comparison(*op, &mut left, &mut right, false)?;
                    let mut result = ExpressionType::resolved(Some(ColumnType::Boolean));
                    result.derivation = ExpressionType::comparison(&left, &right);
                    return Ok(result);
                }
                let ty = self.binary(*op, &mut left, &mut right)?;
                return Ok(ExpressionType::combined(Some(ty), &[left, right]));
            }
            ScalarExpr::Not(inner) => {
                let value = self.boolean_expression(inner, input, subqueries, "NOT")?;
                return Ok(ExpressionType::independent(
                    Some(ColumnType::Boolean),
                    &[value],
                ));
            }
            ScalarExpr::And(items) | ScalarExpr::Or(items) => {
                let context = if matches!(expression, ScalarExpr::And(_)) {
                    "AND"
                } else {
                    "OR"
                };
                let mut selected = Derivation::None;
                for item in items {
                    let value = self.boolean_expression(item, input, subqueries, context)?;
                    selected = selected.combine(value.derivation.error_only());
                }
                let mut result = ExpressionType::resolved(Some(ColumnType::Boolean));
                result.derivation = selected;
                return Ok(result);
            }
            ScalarExpr::IsNull { expr, .. } => {
                let value = self.expression(expr, input, subqueries)?;
                return Ok(ExpressionType::independent(
                    Some(ColumnType::Boolean),
                    &[value],
                ));
            }
            ScalarExpr::UnaryMinus(inner) => {
                let inner = self.expression(inner, input, subqueries)?;
                if inner.ty.is_none() {
                    return Err(crate::type_resolution::ambiguous_prefix_operator(
                        "-", "unknown",
                    ));
                }
                let ty = self.known_type(expression, input, subqueries)?;
                return Ok(ExpressionType::independent(ty, &[inner]));
            }
            ScalarExpr::Array(items) => {
                let mut items = items
                    .iter()
                    .map(|item| self.expression(item, input, subqueries))
                    .collect::<Result<Vec<_>, _>>()?;
                let ty = self
                    .common(crate::type_resolution::CommonTypeContext::Array, &mut items)?
                    .map(|element| ColumnType::Array(Box::new(element)));
                return Ok(ExpressionType::combined(ty, &items));
            }
            ScalarExpr::CompositeRow { items, binding, .. } => {
                let mut selected = Derivation::None;
                for item in items {
                    selected = selected.combine(
                        self.expression(item, input, subqueries)?
                            .derivation
                            .error_only(),
                    );
                }
                let mut result = ExpressionType::resolved(Some(self.type_name(&binding.ty)?));
                result.derivation = selected;
                return Ok(result);
            }
            ScalarExpr::Row(items) => return self.row_expression(items, input, subqueries),
            ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. }
            | ScalarExpr::Func { .. }
            | ScalarExpr::WindowCall { .. } => {
                let mut result = self.compound_expression(expression, input, subqueries)?;
                self.expression_record_fields(&mut result, expression, input, subqueries)?;
                return Ok(result);
            }
            ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default => self.known_type(expression, input, subqueries)?,
        };
        let mut result = ExpressionType::resolved(ty);
        if let Some(oid) = input.input_collation(expression) {
            result.derivation = Derivation::implicit(i64::from(oid));
        }
        self.expression_record_fields(&mut result, expression, input, subqueries)?;
        Ok(result)
    }

    fn expression_record_fields(
        &mut self,
        result: &mut ExpressionType,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        if matches!(result.ty, Some(ColumnType::Record)) {
            result.record_fields = self.scope.bind_record_fields(
                self.routines,
                expression,
                input,
                subqueries,
                &self.parameters.values(),
            )?;
        }
        Ok(())
    }

    fn row_expression(
        &mut self,
        items: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let fields = items
            .iter()
            .map(|item| self.expression(item, input, subqueries))
            .collect::<Result<Vec<_>, _>>()?;
        let mut value = ExpressionType::independent(Some(ColumnType::Record), &fields);
        value.record_fields = Some(fields.iter().map(|field| field.ty.clone()).collect());
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
                let query = subqueries.get(*index).ok_or_else(|| {
                    error("XX000", "scalar subquery slot is outside its plan".into())
                })?;
                self.query(query, Some(input))?;
                ExpressionType::resolved(Some(ColumnType::Boolean))
            }
            ScalarExpr::InSubquery { expr, subquery, .. } => {
                let mut value = self.expression(expr, input, subqueries)?;
                let query = subqueries.get(*subquery).ok_or_else(|| {
                    error("XX000", "scalar subquery slot is outside its plan".into())
                })?;
                let schema = self.query(query, Some(input))?;
                let mut result = ExpressionType::resolved(schema.column_type(0).cloned());
                if let Some(oid) = schema.column_collation(0) {
                    result.derivation = Derivation::implicit(i64::from(oid));
                }
                self.binary(BinaryOp::Equal, &mut value, &mut result)?;
                ExpressionType::combined(Some(ColumnType::Boolean), &[value, result])
            }
            ScalarExpr::Func { .. } => self.function_expression(expression, input, subqueries)?,
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                let mut arguments =
                    self.observe_function_arguments(name, args, &[], input, subqueries)?;
                let selected = self.select_function_arguments(name, None, args, &arguments)?;
                self.coerce_function_arguments(name, None, args, &mut arguments, &selected)?;
                let additional =
                    self.function_modifier_collations(&[], filter.as_deref(), input, subqueries)?;
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
        let within_group = super::super::ordered_calls::uses_ordered_arguments(
            *order_syntax,
            name,
            binding,
            order_by.len(),
        );
        let ordered = if within_group {
            order_by.as_slice()
        } else {
            &[]
        };
        let mut arguments =
            self.observe_function_arguments(name, args, ordered, input, subqueries)?;
        let additional = self.function_modifier_collations(
            if within_group { &[] } else { order_by },
            filter.as_deref(),
            input,
            subqueries,
        )?;
        let selected = if within_group || super::super::ordered_calls::is_ordered_set(name) {
            self.select_ordered_function_arguments(name, binding, args, ordered, &arguments)?
        } else {
            self.select_function_arguments(name, binding, args, &arguments)?
        };
        if let Some(kind) = selected.kind.or_else(|| {
            selected
                .overload
                .as_ref()
                .map(|_| super::super::ordered_calls::Kind::Ordinary)
        }) {
            super::super::ordered_calls::validate(
                name,
                kind,
                super::super::ordered_calls::Modifiers {
                    direct: args.len(),
                    ordered: order_by.len(),
                    within_group,
                    distinct: *distinct,
                    filtered: filter.is_some(),
                },
                &arguments.names,
                &arguments.types(),
            )?;
        }
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
        let selected = selected.overload;
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
        self.check_schema_function(
            expression,
            selected
                .as_ref()
                .map(|selected| &selected.binding)
                .or(binding),
            input,
        )?;
        Ok(super::function_calls::collated_result(
            ty,
            &arguments.values,
            binding,
            additional.diagnostics,
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
    ) -> Result<(), SQLError> {
        self.call_binding(name, binding, args, input, subqueries)
            .map(|_| ())
    }

    fn call_binding(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        let mut arguments = self.observe_function_arguments(name, args, &[], input, subqueries)?;
        let selected = self.select_function_arguments(name, binding, args, &arguments)?;
        self.coerce_function_arguments(name, binding, args, &mut arguments, &selected)?;
        Ok(selected.overload)
    }
}
