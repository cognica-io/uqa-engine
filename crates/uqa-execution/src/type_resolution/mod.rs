//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL type propagation and PostgreSQL-compatible common-type rules.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};
#[cfg(test)]
use uqa_core::Value;

mod array_transform;
mod cast_compatibility;
mod checksum;
mod common;
mod containment;
mod equality;
mod fixed_builtin;
mod functions;
mod gamma;
mod introspection;
mod json_strip;
mod length;
mod md5;
mod operators;
mod overload_resolution;
mod qualified_column;
mod range;
mod reverse;
mod routine_signature;
mod string_binary;

#[doc(hidden)]
pub use checksum::{resolve_checksum_overload, ResolvedChecksumOverload};
pub use common::{
    common_context_expression_type, common_type, effective_overload_argument_type,
    effective_overload_argument_type_with_params, function_call_argument_signature,
    values_column_types, FunctionCallArgumentSignature,
};
pub use equality::{equality_operand_type, foreign_key_operand_type};
#[doc(hidden)]
pub use fixed_builtin::{
    fixed_builtin_return_type, is_function as is_fixed_builtin, resolve_fixed_builtin_call,
    ResolvedFixedBuiltinCall,
};
pub use functions::{builtin_function_argument_targets, builtin_function_type};
#[doc(hidden)]
pub use gamma::{resolve_gamma_overload, ResolvedGammaOverload};
pub use introspection::{bind_type_introspection, bind_type_introspection_with_resolver};
#[doc(hidden)]
pub use json_strip::{resolve_json_strip_overload, ResolvedJsonStripOverload};
#[doc(hidden)]
pub use length::{resolve_length_overload, ResolvedLengthOverload};
#[doc(hidden)]
pub use md5::{resolve_md5_overload, ResolvedMd5Overload};
#[doc(hidden)]
pub use operators::{require_equality_operator, require_ordering_operator};
#[doc(hidden)]
pub use overload_resolution::{
    builtin_binding_matches, builtin_name_matches, canonical_column_type_name,
    canonical_routine_type_name, function_resolution_error, match_builtin_function_overload,
    match_function_signature, rank_function_matches, resolve_local_builtin_overload,
    routine_type_accepts_implicit_cast, routine_type_category, routine_type_is_preferred,
    FunctionParameterDescriptor, MatchedBuiltinFunction, MatchedFunctionSignature,
    RankedFunctionMatch,
};
#[doc(hidden)]
pub use reverse::{resolve_reverse_overload, ResolvedReverseOverload};
#[doc(hidden)]
pub use routine_signature::{
    match_routine_signature, routine_polymorphic_type, MatchedRoutineSignature,
    RoutineCallDescriptor, RoutineCoercionTarget, RoutineParameterDescriptor,
    RoutinePolymorphicFamily, RoutinePolymorphicType, RoutineSignatureMatchError,
    RoutineTypeSubstitutions, RoutineVariadicMode, RoutineVariadicPlan,
};
#[doc(hidden)]
pub use string_binary::{ResolvedStringBinaryOverload, ResolvedTextByteaOverload};

pub trait FunctionTypeResolver: Send + Sync {
    /// Return whether an external runtime callback claims this unbound function
    /// name without exposing a declared SQL return type. Such callbacks must
    /// retain dispatch precedence instead of being rebound to a same-named
    /// built-in overload.
    fn has_untyped_function(&self, _name: &str) -> bool {
        false
    }

    /// Resolve a catalog-owned SQL type name that is not represented by the
    /// built-in [`ColumnType::from_sql_name`] mapping, such as a domain.
    fn resolve_type_name(&self, _name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError>;

    /// Resolve a catalog-backed overload together with the stable binding needed to execute it after built-in and user-defined candidates have been ranked.
    fn resolve_function_overload(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        Ok(None)
    }

    /// Return whether an exact catalog-selected binding can execute in a scalar expression. The conservative default prevents aggregate, procedure, and set-returning routines from being attached to [`ScalarExpr::Func`].
    fn is_scalar_function_binding(&self, _binding: &FunctionBinding) -> Result<bool, SQLError> {
        Ok(false)
    }

    /// Resolve catalog-backed routines and the supplied built-in overloads as
    /// one `PostgreSQL` candidate set. Implementations with catalog visibility
    /// should override this so search-path shadowing and unknown-category
    /// selection happen before a winner is chosen.
    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        _builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
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
pub struct BuiltinFunctionOverload {
    pub name: String,
    pub argument_names: Vec<Option<String>>,
    pub argument_types: Vec<ColumnType>,
    pub default_arguments: usize,
    pub return_type: ColumnType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFunctionOverload {
    pub binding: FunctionBinding,
    pub return_type: ColumnType,
    pub exact_matches: usize,
    pub known_arguments: usize,
    pub preferred_matches: usize,
    pub precedes_pg_catalog: bool,
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

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
pub(super) fn scalar_type_inner(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    if matches!(
        expression,
        ScalarExpr::Func { binding, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch).is_some_and(
                uqa_sql::ast::FunctionDispatch::is_call_argument_marker
            )
    ) {
        let argument = crate::scalar_call_argument(expression)?;
        return scalar_type_inner(argument.value, schema, params, resolver);
    }
    match expression {
        ScalarExpr::Column(column) => {
            if schema.has_unqualified_column(column) || schema.column_is_ambiguous(column) {
                Ok(schema.type_of(column).cloned())
            } else if schema.has_qualifier(column) {
                Ok(Some(ColumnType::Record))
            } else {
                Ok(None)
            }
        }
        ScalarExpr::Position(position) => Ok(schema.column_type(*position).cloned()),
        ScalarExpr::InternalColumn(column) => Ok(schema.internal_type(*column).cloned()),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            qualified_column::resolve(schema, qualifier, column)
        }
        ScalarExpr::Literal(value) => Ok(common::value_type(value)),
        ScalarExpr::Param(index) => Ok(index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .and_then(common::parameter_type)),
        ScalarExpr::Cast { expr, ty } => {
            let source = scalar_type_inner(expr, schema, params, resolver)?;
            let target = match ColumnType::from_sql_name(ty) {
                Ok(ty) => Ok(Some(ty)),
                Err(error @ SQLError::Unsupported(_)) => match resolver {
                    Some(resolver) => resolver
                        .resolve_type_name(ty)?
                        .map_or(Err(error), |ty| Ok(Some(ty))),
                    None => Err(error),
                },
                Err(error) => Err(error),
            }?;
            if let Some(target) = target.as_ref() {
                cast_compatibility::validate_void_cast(source.as_ref(), target)?;
            }
            Ok(target)
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
            let value = scalar_type_inner(expr, schema, params, resolver)?;
            let low = scalar_type_inner(low, schema, params, resolver)?;
            let high = scalar_type_inner(high, schema, params, resolver)?;
            operators::binary_result_type(
                uqa_sql::ast::BinaryOp::GreaterEqual,
                value.as_ref(),
                low.as_ref(),
            )?;
            operators::binary_result_type(
                uqa_sql::ast::BinaryOp::LessEqual,
                value.as_ref(),
                high.as_ref(),
            )?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InList { expr, list, .. } => {
            let needle = scalar_type_inner(expr, schema, params, resolver)?;
            for item in list {
                let candidate = scalar_type_inner(item, schema, params, resolver)?;
                operators::binary_result_type(
                    uqa_sql::ast::BinaryOp::Equal,
                    needle.as_ref(),
                    candidate.as_ref(),
                )?;
            }
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            let needle = scalar_type_inner(expr, schema, params, resolver)?;
            let candidate = resolver
                .map(|resolver| resolver.resolve_scalar_subquery_type(*subquery, schema, params))
                .transpose()?
                .flatten();
            operators::binary_result_type(
                uqa_sql::ast::BinaryOp::Equal,
                needle.as_ref(),
                candidate.as_ref(),
            )?;
            Ok(Some(ColumnType::Boolean))
        }
        ScalarExpr::Exists { .. } => Ok(Some(ColumnType::Boolean)),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let simple = base.is_some();
            let base_type = base
                .as_deref()
                .map(|base| scalar_type_inner(base, schema, params, resolver))
                .transpose()?
                .flatten();
            let mut result = None;
            for (condition, value) in when {
                let condition_type = scalar_type_inner(condition, schema, params, resolver)?;
                if simple {
                    operators::binary_result_type(
                        uqa_sql::ast::BinaryOp::Equal,
                        base_type.as_ref(),
                        condition_type.as_ref(),
                    )?;
                }
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
            distinct,
            order_by,
            filter,
        } => {
            if let Some(uqa_sql::ast::FunctionResolutionError::UndefinedFunction { signature }) =
                binding
                    .as_ref()
                    .and_then(|binding| binding.resolution_error.as_ref())
            {
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: format!("function {signature} does not exist"),
                });
            }
            if let Some(filter) = filter {
                scalar_type_inner(filter, schema, params, resolver)?;
            }
            if *distinct {
                for argument in args {
                    if let Some(ty) = scalar_type_inner(argument, schema, params, resolver)? {
                        require_equality_operator(&ty)?;
                    }
                }
            }
            for order in order_by {
                if let Some(ty) = scalar_type_inner(&order.expr, schema, params, resolver)? {
                    require_ordering_operator(&ty)?;
                }
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
                if let Some(ty) = scalar_type_inner(expression, schema, params, resolver)? {
                    require_equality_operator(&ty)?;
                }
            }
            for order in &spec.order_by {
                if let Some(ty) = scalar_type_inner(&order.expr, schema, params, resolver)? {
                    require_ordering_operator(&ty)?;
                }
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
        ScalarExpr::QualifiedStar(qualifier) if schema.has_qualifier(qualifier) => {
            Ok(Some(ColumnType::Record))
        }
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) | ScalarExpr::Default => Ok(None),
    }
}

#[cfg(test)]
mod tests;
