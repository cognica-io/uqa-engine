//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL type propagation and PostgreSQL-compatible common-type rules.

use crate::ast::{ColumnType, FunctionBinding};
use crate::{SQLError, SQLParam};

use crate::schema::ScalarTypeSchema;
use crate::{RowSchema, ScalarExpr};
#[cfg(test)]
use uqa_core::Value;

mod array_transform;
mod call;
mod cast_compatibility;
mod checksum;
mod common;
pub(crate) use common::value_type;
pub(crate) use common::value_type_with_control;
mod containment;
mod equality;
mod fixed_builtin;
mod functions;
mod gamma;
mod inference;
use inference::scalar_type_inner_with_control;
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
mod scalar_input;
pub use scalar_input::{
    scalar_integer_operation_width, scalar_integer_operation_width_with_control,
    scalar_operand_type_name, scalar_operand_type_name_with_control,
};
mod string_binary;

pub(crate) use cast_compatibility::cast_catalog_entry_with_control;
pub use cast_compatibility::{
    assignment_type_compatible, cast_catalog_entry, explicit_type_compatible, CastCatalogEntry,
    CastMethod,
};
#[doc(hidden)]
pub use checksum::{resolve_checksum_overload, ResolvedChecksumOverload};
pub use common::{
    common_context_expression_type, common_type, effective_overload_argument_type,
    effective_overload_argument_type_with_params, function_call_argument_signature,
    values_column_types, FunctionCallArgumentSignature,
};
pub use equality::{
    equality_operand_type, equality_operand_type_with_control, foreign_key_operand_type,
};
#[doc(hidden)]
pub use fixed_builtin::{
    fixed_builtin_return_type, fixed_builtin_return_type_with_control,
    is_function as is_fixed_builtin, resolve_fixed_builtin_call, ResolvedFixedBuiltinCall,
};
pub use functions::{builtin_function_argument_targets, builtin_function_type};
#[doc(hidden)]
pub use gamma::{resolve_gamma_overload, ResolvedGammaOverload};
pub use introspection::{
    bind_type_introspection, bind_type_introspection_with_control,
    bind_type_introspection_with_resolver,
};
#[doc(hidden)]
pub use json_strip::{resolve_json_strip_overload, ResolvedJsonStripOverload};
#[doc(hidden)]
pub use length::{resolve_length_overload, ResolvedLengthOverload};
#[doc(hidden)]
pub use md5::{resolve_md5_overload, ResolvedMd5Overload};
#[doc(hidden)]
pub use operators::{
    binary_operator_by_oid, binary_operator_catalog_entry, binary_operator_types,
    binary_operator_types_with_control, binary_result_type, binary_result_type_with_control,
    numeric_operator_types, numeric_operator_types_with_control, require_equality_operator,
    require_ordering_operator, unary_minus_catalog_entry, unary_operator_by_oid,
    BinaryOperatorCatalogEntry, NumericOperatorTypes, UnaryOperatorCatalogEntry,
};
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
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, None)
}

/// Infer a scalar type from borrowed schema and parameter metadata while retaining every constructed type and temporary buffer under the supplied allowance.
pub fn scalar_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    scalar_type_inner_with_control(expression, schema, params, None, control)
}

/// Preserve unknown-literal and domain rules when selecting an operator's common type without a catalog callback.
pub fn common_context_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    common::common_context_expression_type_with_control(expression, schema, params, None, control)
}

pub fn scalar_type_with_resolver(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner(expression, schema, params, Some(resolver))
}

pub(super) fn scalar_type_inner(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    scalar_type_inner_with_control(
        expression,
        schema,
        params,
        resolver,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|ty| {
        ty.map(|ty| {
            ty.into_uncontrolled()
                .expect("ordinary scalar inference has no reservation")
        })
    })
}

#[cfg(test)]
mod tests;

mod declaration;
pub use declaration::resolve_declared_column_type;

mod coercion;
pub use coercion::coerce_common_context_value;
