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
use uqa_core::Value;

impl Preparation<'_> {
    /// The common type of the inputs of the construct `context`, as `select_common_type` chooses it, with the unknown inputs coerced to it.
    pub(super) fn common(
        &mut self,
        context: crate::type_resolution::CommonTypeContext,
        values: &mut [ExpressionType],
    ) -> Result<Option<ColumnType>, SQLError> {
        let mut common = None;
        for value in values.iter() {
            if let Some(ty) = &value.ty {
                common = Some(match common {
                    Some(previous) => {
                        crate::type_resolution::common_type_in(context, &previous, ty)?
                    }
                    None => ty.clone(),
                });
            }
        }
        if common.is_none() && values.iter().any(ExpressionType::is_deferred) {
            return Ok(None);
        }
        let common = common.unwrap_or(ColumnType::Text);
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
        let [left_target, right_target, result] =
            crate::type_resolution::binary_operator_types(op, left.ty.as_ref(), right.ty.as_ref())?;
        self.parameters.coerce_unknown(left, &left_target)?;
        self.parameters.coerce_unknown(right, &right_target)?;
        Ok(result)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered dispatch covers every scalar expression"
    )]
    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        if let Some(index) = self.scope.routine_parameter_reference(expression, input) {
            return self.parameters.reference(index);
        }
        self.check_schema_subquery(expression)?;
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
            ScalarExpr::Param(index) => return self.parameters.reference(*index),
            ScalarExpr::Literal(Value::Null) => return Ok(ExpressionType::unknown()),
            ScalarExpr::Literal(Value::Str(text)) => {
                return Ok(ExpressionType::unknown_literal(expression, text.clone()));
            }
            ScalarExpr::Cast { expr, ty, .. } => {
                Some(self.cast_expression(expr, ty, input, subqueries)?)
            }
            ScalarExpr::Binary { op, lhs, rhs } => {
                let mut left = self.expression(lhs, input, subqueries)?;
                let mut right = self.expression(rhs, input, subqueries)?;
                Some(self.binary(*op, &mut left, &mut right)?)
            }
            ScalarExpr::Not(inner) => {
                self.require_boolean(inner, input, subqueries, "NOT")?;
                Some(ColumnType::Boolean)
            }
            ScalarExpr::And(items) | ScalarExpr::Or(items) => {
                let context = if matches!(expression, ScalarExpr::And(_)) {
                    "AND"
                } else {
                    "OR"
                };
                for item in items {
                    self.require_boolean(item, input, subqueries, context)?;
                }
                Some(ColumnType::Boolean)
            }
            ScalarExpr::IsNull { expr, .. } => {
                self.expression(expr, input, subqueries)?;
                Some(ColumnType::Boolean)
            }
            ScalarExpr::UnaryMinus(inner) => {
                let inner = self.expression(inner, input, subqueries)?;
                if inner.ty.is_none() {
                    return Err(crate::type_resolution::ambiguous_prefix_operator(
                        "-", "unknown",
                    ));
                }
                self.known_type(expression, input, subqueries)?
            }
            ScalarExpr::Array(items) => {
                let mut items = items
                    .iter()
                    .map(|item| self.expression(item, input, subqueries))
                    .collect::<Result<Vec<_>, _>>()?;
                self.common(crate::type_resolution::CommonTypeContext::Array, &mut items)?
                    .map(|element| ColumnType::Array(Box::new(element)))
            }
            ScalarExpr::CompositeRow { items, binding, .. } => {
                for item in items {
                    self.expression(item, input, subqueries)?;
                }
                Some(self.type_name(&binding.ty)?)
            }
            ScalarExpr::Row(items) => return self.row_expression(items, input, subqueries),
            ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. }
            | ScalarExpr::Func { .. }
            | ScalarExpr::WindowCall { .. } => {
                self.compound_expression(expression, input, subqueries)?
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
        if matches!(result.ty, Some(ColumnType::Record)) {
            result.record_fields = self.scope.bind_record_fields(
                self.routines,
                expression,
                input,
                subqueries,
                &self.parameters.values(),
            )?;
        }
        Ok(result)
    }

    fn row_expression(
        &mut self,
        items: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let fields = items
            .iter()
            .map(|item| {
                self.expression(item, input, subqueries)
                    .map(|value| value.ty)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut value = ExpressionType::resolved(Some(ColumnType::Record));
        value.record_fields = Some(fields.into());
        Ok(value)
    }

    fn compound_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(match expression {
            ScalarExpr::Between { expr, low, high } => {
                let mut value = self.expression(expr, input, subqueries)?;
                let mut low = self.expression(low, input, subqueries)?;
                let mut high = self.expression(high, input, subqueries)?;
                self.binary(BinaryOp::GreaterEqual, &mut value, &mut low)?;
                self.binary(BinaryOp::LessEqual, &mut value, &mut high)?;
                Some(ColumnType::Boolean)
            }
            ScalarExpr::InList { .. } => {
                self.in_list(expression, input, subqueries)?;
                Some(ColumnType::Boolean)
            }
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
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            } => {
                let query = subqueries.get(*index).ok_or_else(|| {
                    error("XX000", "scalar subquery slot is outside its plan".into())
                })?;
                let schema = self.query(query, Some(input))?;
                if matches!(expression, ScalarExpr::Exists { .. }) {
                    Some(ColumnType::Boolean)
                } else {
                    schema.column_type(0).cloned()
                }
            }
            ScalarExpr::InSubquery { expr, subquery, .. } => {
                let mut value = self.expression(expr, input, subqueries)?;
                let query = subqueries.get(*subquery).ok_or_else(|| {
                    error("XX000", "scalar subquery slot is outside its plan".into())
                })?;
                let schema = self.query(query, Some(input))?;
                let mut result = ExpressionType::resolved(schema.column_type(0).cloned());
                self.binary(BinaryOp::Equal, &mut value, &mut result)?;
                Some(ColumnType::Boolean)
            }
            ScalarExpr::Func { .. } => self.function_expression(expression, input, subqueries)?,
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                self.call(name, None, args, input, subqueries)?;
                if let Some(filter) = filter {
                    self.require_boolean(filter, input, subqueries, "FILTER")?;
                }
                self.check_schema_window(name, (args, filter.is_some(), *modifiers), input)?;
                if self.schema_expression.is_some() {
                    self.window_specification(spec, input, subqueries)?;
                }
                self.known_type(expression, input, subqueries)?
            }
            _ => self.known_type(expression, input, subqueries)?,
        })
    }

    fn function_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
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
        if !within_group {
            for order in order_by {
                self.expression(&order.expr, input, subqueries)?;
            }
        }
        if let Some(filter) = filter {
            self.require_boolean(filter, input, subqueries, "FILTER")?;
        }
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
        self.coerce_function_arguments(name, binding, &mut arguments, &selected)?;
        let selected = selected.overload;
        let ty = match selected
            .as_ref()
            .filter(|selected| !selected.binding.builtin)
        {
            Some(selected) => Some(selected.return_type.clone()),
            None => self.known_type(expression, input, subqueries)?,
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
        Ok(ty)
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
        self.coerce_function_arguments(name, binding, &mut arguments, &selected)?;
        Ok(selected.overload)
    }
}
