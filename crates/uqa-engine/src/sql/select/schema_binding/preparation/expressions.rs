//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar coercion contexts used while preparing a statement.

use super::{
    error, ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, FunctionBinding};

impl Preparation<'_> {
    pub(super) fn common(&mut self, values: &mut [ExpressionType]) -> Result<ColumnType, SQLError> {
        let mut common = None;
        for value in values.iter() {
            if let Some(ty) = &value.ty {
                common = Some(match common {
                    Some(previous) => uqa_execution::common_type(&previous, ty)?,
                    None => ty.clone(),
                });
            }
        }
        let common = common.unwrap_or(ColumnType::Text);
        for value in values {
            self.parameters.coerce_unknown(value, &common)?;
        }
        Ok(common)
    }

    pub(super) fn binary(
        &mut self,
        op: BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
    ) -> Result<ColumnType, SQLError> {
        let [left_target, right_target, result] =
            uqa_execution::type_resolution::binary_operator_types(
                op,
                left.ty.as_ref(),
                right.ty.as_ref(),
            )?;
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
        let ty = match expression {
            ScalarExpr::Param(index) => return self.parameters.reference(*index),
            ScalarExpr::Literal(Value::Null | Value::Str(_)) => None,
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
                    return Err(error("42725", "operator is not unique: - unknown".into()));
                }
                self.known_type(expression, input, subqueries)?
            }
            ScalarExpr::Array(items) => {
                let mut items = items
                    .iter()
                    .map(|item| self.expression(item, input, subqueries))
                    .collect::<Result<Vec<_>, _>>()?;
                Some(ColumnType::Array(Box::new(self.common(&mut items)?)))
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
                self.common(&mut values)?;
                Some(ColumnType::Boolean)
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => Some(self.case_expression(
                base.as_deref(),
                when,
                else_branch.as_deref(),
                input,
                subqueries,
            )?),
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
                self.call(name, binding.as_ref(), args, input, subqueries)?;
                for order in order_by {
                    self.expression(&order.expr, input, subqueries)?;
                }
                if let Some(filter) = filter {
                    self.require_boolean(filter, input, subqueries, "FILTER")?;
                }
                self.known_type(expression, input, subqueries)?
            }
            ScalarExpr::WindowCall { name, args, spec } => {
                self.call(name, None, args, input, subqueries)?;
                self.window_specification(spec, input, subqueries)?;
                self.known_type(expression, input, subqueries)?
            }
            _ => self.known_type(expression, input, subqueries)?,
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
        let arguments = uqa_execution::scalar_call_arguments(args)?;
        let mut observed = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                if crate::sql::is_semantic_field_argument(name, args, index)? {
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
        if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
            if name == "nullif" {
                if let [left, right] = observed.as_mut_slice() {
                    self.binary(BinaryOp::Equal, left, right)?;
                }
            } else {
                self.common(&mut observed)?;
            }
            return Ok(());
        }
        let (selected, positions) = if let Some(fixed) = uqa_execution::resolve_fixed_builtin_call(
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
        let targets = if let Some(selected) = selected {
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
            uqa_execution::type_resolution::builtin_function_argument_targets(name, &types)
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
        Ok(())
    }
}
