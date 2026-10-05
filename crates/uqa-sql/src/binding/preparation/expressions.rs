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

    /// `transformAExprIn`: the needle and the list compare at their common type when they have one, and otherwise each item is compared with the needle through its own `=` operator.
    fn in_list(&mut self, values: &mut [ExpressionType]) -> Result<(), SQLError> {
        let types = values
            .iter()
            .map(|value| value.ty.as_ref())
            .collect::<Vec<_>>();
        if crate::type_resolution::select_common_input_type(&types)?.is_some() {
            self.common(crate::type_resolution::CommonTypeContext::In, values)?;
            return Ok(());
        }
        let Some((needle, items)) = values.split_first_mut() else {
            return Ok(());
        };
        for item in items {
            self.binary(BinaryOp::Equal, needle, item)?;
        }
        Ok(())
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

    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        self.check_transform_subquery(expression)?;
        let ty = match expression {
            ScalarExpr::Param(index) => return self.parameters.reference(*index),
            ScalarExpr::Literal(Value::Null) => return Ok(ExpressionType::unknown()),
            ScalarExpr::Literal(Value::Str(text)) => {
                return Ok(ExpressionType::unknown_literal(text.clone()));
            }
            ScalarExpr::Cast { expr, ty } => {
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
            ScalarExpr::Row(items) => {
                for item in items {
                    self.expression(item, input, subqueries)?;
                }
                Some(ColumnType::Record)
            }
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
        Ok(ExpressionType::resolved(ty))
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
            ScalarExpr::InList { expr, list, .. } => {
                let mut values = vec![self.expression(expr, input, subqueries)?];
                for item in list {
                    values.push(self.expression(item, input, subqueries)?);
                }
                self.in_list(&mut values)?;
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
            ScalarExpr::Func {
                name,
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                let selected =
                    self.call_binding(name, binding.as_ref(), args, input, subqueries)?;
                for order in order_by {
                    self.expression(&order.expr, input, subqueries)?;
                }
                if let Some(filter) = filter {
                    self.require_boolean(filter, input, subqueries, "FILTER")?;
                }
                let ty = self.known_type(expression, input, subqueries)?;
                self.check_transform_function(
                    expression,
                    selected.as_ref().or(binding.as_ref()),
                    input,
                )?;
                ty
            }
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
                self.check_transform_window(name, (args, filter.is_some(), *modifiers), input)?;
                self.window_specification(spec, input, subqueries)?;
                self.known_type(expression, input, subqueries)?
            }
            _ => self.known_type(expression, input, subqueries)?,
        })
    }

    fn check_transform_subquery(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        if self.transform_catalog.is_some()
            && matches!(
                expression,
                ScalarExpr::ScalarSubquery(_)
                    | ScalarExpr::Exists { .. }
                    | ScalarExpr::InSubquery { .. }
            )
        {
            return Err(error(
                "0A000",
                "cannot use subquery in transform expression".into(),
            ));
        }
        Ok(())
    }

    fn check_transform_window(
        &self,
        name: &str,
        call: (&[ScalarExpr], bool, crate::ast::WindowCallModifiers),
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        if self.transform_catalog.is_none() {
            return Ok(());
        }
        super::super::analysis::validate_window_function(
            self.routines,
            name,
            call,
            input,
            &[],
            self.routines,
        )?;
        Err(error(
            "42P20",
            "window functions are not allowed in transform expressions".into(),
        ))
    }

    fn check_transform_function(
        &self,
        expression: &ScalarExpr,
        selected: Option<&FunctionBinding>,
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.transform_catalog else {
            return Ok(());
        };
        let ScalarExpr::Func { name, args, .. } = expression else {
            unreachable!("transform call context");
        };
        let scalar = selected
            .map(|binding| self.routines.is_scalar_function_binding(binding))
            .transpose()?
            .unwrap_or(false);
        if !scalar
            && (crate::semantics::is_builtin_aggregate_call(name, selected)
                || catalog.is_registered_aggregate(name))
        {
            return Err(error(
                "42803",
                "aggregate functions are not allowed in transform expressions".into(),
            ));
        }
        if crate::semantics::sets::validation::function_may_return_set(
            catalog,
            catalog,
            name,
            selected,
            args,
            input,
            &[],
        )? {
            return Err(error(
                "0A000",
                "set-returning functions are not allowed in transform expressions".into(),
            ));
        }
        Ok(())
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
    ) -> Result<Option<FunctionBinding>, SQLError> {
        let arguments = crate::scalar_call_arguments(args)?;
        let mut observed = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                if crate::semantics::is_semantic_field_argument(name, args, index)? {
                    Ok(ExpressionType::resolved(Some(ColumnType::Text)))
                } else {
                    self.expression(argument.value, input, subqueries)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let types = observed
            .iter()
            .map(|value| value.ty.clone())
            .collect::<Vec<_>>();
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_string))
            .collect::<Vec<_>>();
        let variadic = arguments.iter().any(|argument| argument.explicit_variadic);
        if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
            binding.and_then(|binding| binding.dispatch)
        {
            let selected = crate::type_resolution::numeric_operator_types(operator, &types)?;
            for (value, target) in observed.iter_mut().zip(&selected.arguments) {
                self.parameters.coerce_unknown(value, target)?;
            }
            return Ok(binding.cloned());
        }
        if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
            if name == "nullif" {
                if let [left, right] = observed.as_mut_slice() {
                    self.binary(BinaryOp::Equal, left, right)?;
                }
            } else {
                self.common(
                    crate::type_resolution::CommonTypeContext::function(name)
                        .unwrap_or(crate::type_resolution::CommonTypeContext::Coalesce),
                    &mut observed,
                )?;
            }
            return Ok(binding.cloned());
        }
        let (selected, positions) = if let Some(fixed) = crate::resolve_fixed_builtin_call(
            name,
            binding,
            &names,
            &types,
            variadic,
            Some(self.routines),
        )? {
            (Some(fixed.selected), fixed.builtin_argument_positions)
        } else {
            (
                self.routines
                    .resolve_function_overload(name, binding, &names, &types, variadic)?,
                None,
            )
        };
        let targets = if let Some(selected) = &selected {
            let types = selected
                .binding
                .invocation
                .as_ref()
                .map_or(&selected.binding.argument_types, |invocation| {
                    &invocation.argument_targets
                });
            types
                .iter()
                .map(|name| self.type_name(name).map(Some))
                .collect::<Result<Vec<_>, _>>()?
        } else if matches!(name, "cypher" | "ag_catalog.cypher") {
            [
                ColumnType::Name,
                ColumnType::Text,
                self.type_name("ag_catalog.agtype")?,
            ]
            .into_iter()
            .take(types.len())
            .map(Some)
            .collect()
        } else {
            crate::type_resolution::builtin_function_argument_targets(name, &types)
        };
        for (index, value) in observed.iter_mut().enumerate() {
            let position = positions
                .as_ref()
                .map_or(index, |positions| positions[index]);
            let target = targets.get(position).and_then(Option::as_ref);
            if let Some(target) = target {
                self.parameters.coerce_unknown(value, target)?;
            }
        }
        Ok(selected.map(|selected| selected.binding))
    }
}
