//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL type propagation and PostgreSQL-compatible common-type rules.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

#[cfg(test)]
use uqa_core::Value;
#[cfg(test)]
use uqa_sql::expr::{
    RANDOM_INT4_FUNCTION, RANDOM_INT8_FUNCTION, RANDOM_NUMERIC_FUNCTION, TO_BIN_INT4_FUNCTION,
    TO_BIN_INT8_FUNCTION, TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION, TO_OCT_INT4_FUNCTION,
    TO_OCT_INT8_FUNCTION,
};

use crate::{RowSchema, ScalarExpr};

mod array_transform;
mod common;
mod containment;
mod equality;
mod functions;
mod integer_base;
mod introspection;
mod operators;
mod qualified_column;
mod random_range;
mod uuid;

pub use common::{common_context_expression_type, common_type, values_column_types};
pub use equality::equality_operand_type;
pub use functions::{builtin_function_argument_targets, builtin_function_type};
pub use introspection::{bind_type_introspection, bind_type_introspection_with_resolver};

pub trait FunctionTypeResolver: Send + Sync {
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
    ) -> Result<Option<ColumnType>, SQLError>;

    /// Resolve a catalog-backed overload together with the stable binding needed to execute it after built-in and user-defined candidates have been ranked.
    fn resolve_function_overload(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        Ok(None)
    }

    /// Resolve the declared first-column type of a physical scalar-subquery slot when the owning execution context carries its plan arena.
    fn resolve_scalar_subquery_type(
        &self,
        _subquery: crate::SubqueryId,
        _outer_schema: &RowSchema,
        _params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFunctionOverload {
    pub binding: FunctionBinding,
    pub return_type: ColumnType,
    pub exact_matches: usize,
    pub known_arguments: usize,
}

impl ResolvedFunctionOverload {
    #[must_use]
    pub fn is_exact_for_known_arguments(&self) -> bool {
        self.known_arguments > 0 && self.exact_matches == self.known_arguments
    }
}

pub fn scalar_type(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, None)
}

pub fn scalar_type_with_resolver(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, Some(resolver))
}

pub(super) fn scalar_type_inner(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    match expression {
        ScalarExpr::Column(column) => Ok(schema.type_of(column).cloned()),
        ScalarExpr::Position(position) => Ok(schema.column_type(*position).cloned()),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            qualified_column::resolve(schema, qualifier, column)
        }
        ScalarExpr::Literal(value) => Ok(common::value_type(value)),
        ScalarExpr::Param(index) => Ok(index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .and_then(common::parameter_type)),
        ScalarExpr::Cast { expr, ty } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            ColumnType::from_sql_name(ty).map(Some)
        }
        ScalarExpr::Array(items) => {
            let mut element = None;
            for item in items {
                element = common::merge_optional_types(
                    element,
                    common::common_context_expression_type(item, schema, params, resolver)?,
                )?;
            }
            Ok(element.map(|element| ColumnType::Array(Box::new(element))))
        }
        ScalarExpr::Row(items) => {
            for item in items {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Record))
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = scalar_type_inner(lhs, schema, params, resolver)?;
            let right = scalar_type_inner(rhs, schema, params, resolver)?;
            operators::binary_result_type(*op, left.as_ref(), right.as_ref())
        }
        ScalarExpr::UnaryMinus(inner) => scalar_type_inner(inner, schema, params, resolver)?
            .map_or(Ok(None), |ty| {
                operators::unary_minus_result_type(&ty).map(Some)
            }),
        ScalarExpr::Not(inner) | ScalarExpr::IsNull { expr: inner, .. } => {
            scalar_type_inner(inner, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::Between { expr, low, high } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            scalar_type_inner(low, schema, params, resolver)?;
            scalar_type_inner(high, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InList { expr, list, .. } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            for item in list {
                scalar_type_inner(item, schema, params, resolver)?;
            }
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InSubquery { expr, .. } => {
            scalar_type_inner(expr, schema, params, resolver)?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::Exists { .. } => Ok(Some(ColumnType::Boolean)),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                scalar_type_inner(base, schema, params, resolver)?;
            }
            let mut result = None;
            for (condition, value) in when {
                scalar_type_inner(condition, schema, params, resolver)?;
                result = common::merge_optional_types(
                    result,
                    common::common_context_expression_type(value, schema, params, resolver)?,
                )?;
            }
            if let Some(value) = else_branch {
                result = common::merge_optional_types(
                    result,
                    common::common_context_expression_type(value, schema, params, resolver)?,
                )?;
            }
            Ok(result)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            if let Some(filter) = filter {
                scalar_type_inner(filter, schema, params, resolver)?;
            }
            functions::builtin_function_type_inner(
                name,
                binding.as_ref(),
                args,
                order_by,
                schema,
                params,
                resolver,
            )
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            for expression in &spec.partition_by {
                scalar_type_inner(expression, schema, params, resolver)?;
            }
            for order in &spec.order_by {
                scalar_type_inner(&order.expr, schema, params, resolver)?;
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    match bound {
                        crate::ScalarFrameBound::Preceding(expression)
                        | crate::ScalarFrameBound::Following(expression) => {
                            scalar_type_inner(expression, schema, params, resolver)?;
                        }
                        crate::ScalarFrameBound::UnboundedPreceding
                        | crate::ScalarFrameBound::UnboundedFollowing
                        | crate::ScalarFrameBound::CurrentRow => {}
                    }
                }
            }
            functions::builtin_function_type_inner(name, None, args, &[], schema, params, resolver)
        }
        ScalarExpr::ScalarSubquery(subquery) => resolver.map_or(Ok(None), |resolver| {
            resolver.resolve_scalar_subquery_type(*subquery, schema, params)
        }),
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) | ScalarExpr::Default => Ok(None),
    }
}

#[cfg(test)]
mod tests;
