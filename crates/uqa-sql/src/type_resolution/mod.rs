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
use uqa_core::Value;

mod array_transform;
pub(crate) mod between;
pub(crate) mod boolean;
pub(crate) use array_transform::resolve_array_transform_call;
mod call;
pub use call::{FunctionBindingParts, OwnedBindingCall};
mod cast_compatibility;
mod checksum;
mod common;
pub(crate) use common::{function_arguments_have_runtime_types, runtime_input};
pub(crate) use common::{parameter_type_matches, parameter_type_with_control};
pub(crate) mod concatenation;
pub(crate) use common::literal_input_type_with_control;
pub use common::runtime_value_type_with_control;
pub(crate) use common::select_common_input_type_in_with_control;
pub(crate) use common::value_type;
pub(crate) use common::value_type_with_control;
pub(crate) use common::{array_element_type, base_type};
mod containment;
pub(crate) mod enums;
mod equality;
pub(crate) mod field_selection;
pub use field_selection::relation_column as relation_field_column;
mod fixed_builtin;
pub(crate) use fixed_builtin::overloads as fixed_builtin_overloads;
mod function_inputs;
mod functions;
pub(crate) use function_inputs::{call_type_from_observations, CallInputTypes};
mod gamma;
mod inference;
mod production;
use inference::scalar_type_inner_with_control;
mod introspection;
pub(crate) mod json_path;
mod json_strip;
mod length;
mod md5;
pub(crate) mod membership;
mod operator_errors;
pub(crate) mod row_comparison;
pub(crate) mod row_distinct;
pub use operator_errors::{
    ambiguous_binary_operator, ambiguous_prefix_operator, undefined_binary_operator,
    undefined_binary_operator_named, undefined_prefix_operator,
};
mod operators;
pub(crate) use operators::comparison::ComparisonSupport;
pub use operators::comparison::{
    comparison_key_is_constant, equality_types_share_hash_key, type_supports_hashing,
    type_supports_ordering, ComparisonKeySupport,
};
mod overload_resolution;
pub use overload_resolution::canonical_routine_type_name_with_control;
pub(crate) use overload_resolution::resolve_local_builtin_call_with_control;
mod qualified_column;
mod range;
mod range_offsets;
pub use range_offsets::range_frame_offset_type;
mod reverse;
mod routine_signature;
mod scalar_input;
mod stored_constants;
pub use scalar_input::{
    scalar_array_element_oid_with_control, scalar_array_element_type_with_control,
    scalar_array_input_type_with_control, scalar_cast_source_type_name_with_control,
    scalar_integer_operation_width, scalar_integer_operation_width_with_control,
    scalar_operand_type_name, scalar_operand_type_name_with_control,
    scalar_operand_type_with_engine_and_control, scalar_record_field_type_with_control,
};
pub(crate) use stored_constants::store_syntax_operand_coercions;
pub use stored_constants::{
    contains_unknown_literal, fold_stored_enum_constants, store_operand_coercions,
    stored_enum_constant,
};
pub(crate) mod pattern;
mod string_binary;

pub use cast_compatibility::{
    assignment_type_compatible, assignment_type_compatible_with_control, cast_catalog_entry,
    cast_volatility, cast_volatility_with_control, explicit_type_compatible, immutable_cast_type,
    pg_catalog_builtin_type, pg_catalog_builtin_type_with_control, CastCatalogEntry, CastMethod,
};
pub(crate) use cast_compatibility::{cast_catalog_entry_with_control, preserves_record_identity};
#[doc(hidden)]
pub use checksum::{resolve_checksum_overload, ResolvedChecksumOverload};
pub use common::{
    common_context_expression_type, common_type, common_type_in, common_type_in_with_control,
    effective_overload_argument_type, effective_overload_argument_type_with_params,
    function_call_argument_signature, select_common_input_type, values_column_types,
    CommonTypeContext, FunctionCallArgumentSignature,
};
pub use equality::{
    equality_operand_type, equality_operand_type_with_control, foreign_key_operand_type,
};
#[doc(hidden)]
pub use fixed_builtin::{
    fixed_builtin_return_type, fixed_builtin_return_type_with_control,
    is_function as is_fixed_builtin, resolve_fixed_builtin_call, selected_builtin_dispatch,
    ResolvedFixedBuiltinCall,
};
pub use functions::{
    builtin_function_argument_targets, builtin_function_type, builtin_function_type_with_resolver,
};
#[doc(hidden)]
pub use gamma::{resolve_gamma_overload, ResolvedGammaOverload};
pub use introspection::{
    bind_type_introspection, bind_type_introspection_owned, bind_type_introspection_with_control,
    bind_type_introspection_with_resolver, validate_catalog_literals,
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
    binary_operator_types_with_control, binary_operator_volatility,
    binary_operator_volatility_with_control, binary_result_type, binary_result_type_with_control,
    numeric_operator_types, numeric_operator_types_with_control, require_equality_operator,
    require_ordering_operator, unary_minus_catalog_entry, unary_operator_by_oid,
    BinaryOperatorCatalogEntry, NumericOperatorTypes, UnaryOperatorCatalogEntry,
};
pub(crate) use operators::{named_binary_operator_catalog_entry, named_binary_operator_types};
#[doc(hidden)]
pub use overload_resolution::{
    builtin_binding_matches, builtin_name_matches, canonical_column_type_name,
    canonical_routine_type_name, function_resolution_error, match_builtin_function_call,
    match_builtin_function_overload, match_function_signature, parse_enum_type_identity,
    rank_function_matches, resolve_local_builtin_overload, routine_type_accepts_implicit_cast,
    routine_type_category, routine_type_is_preferred, FunctionParameterDescriptor,
    MatchedBuiltinFunction, MatchedFunctionSignature, RankedFunctionMatch,
};
pub(crate) use overload_resolution::{
    match_builtin_with_control, rank_function_matches_with_control, Candidates, RankingCandidates,
};
#[doc(hidden)]
pub use reverse::{resolve_reverse_overload, ResolvedReverseOverload};
#[doc(hidden)]
pub(crate) use routine_signature::{
    match_routine_candidate, match_routine_candidate_with_control,
    routine_polymorphic_type_with_control, RoutineInvocationParts, RoutineSignatureProductionError,
};
pub use routine_signature::{
    match_routine_signature, routine_polymorphic_type, MatchedRoutineSignature,
    RoutineCallDescriptor, RoutineCoercionTarget, RoutineParameterDescriptor,
    RoutinePolymorphicFamily, RoutinePolymorphicType, RoutineSignatureMatchError,
    RoutineTypeSubstitutions, RoutineVariadicMode, RoutineVariadicPlan,
};
#[doc(hidden)]
pub use string_binary::{ResolvedStringBinaryOverload, ResolvedTextByteaOverload};

pub trait FunctionTypeResolver: Send + Sync {
    /// Format a selected type using the binding catalog's namespace visibility.
    fn format_type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.sql_name())
    }

    /// Retain a custom resolver's result in the caller's allowance. Native catalogs override this method to admit their own construction rather than copy an external result.
    fn format_type_name_with_control(
        &self,
        ty: &ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        control.check()?;
        let name = self.format_type_name(ty)?;
        if control.budget().is_none() {
            Ok(control.finish(name, None)?)
        } else {
            control.copy_text(&name).map_err(Into::into)
        }
    }

    /// The default adapts an external catalog's owned result; project-owned catalogs admit their result in the producing implementation.
    fn resolve_type_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        control.check()?;
        production::external_type(self.resolve_type_name(name)?, control)
    }

    /// Return whether an external runtime callback claims this unbound function
    /// name without exposing a declared SQL return type. Such callbacks must
    /// retain dispatch precedence instead of being rebound to a same-named
    /// built-in overload.
    fn has_untyped_function(&self, _name: &str) -> bool {
        false
    }

    /// Observe an external callback's dispatch precedence while retaining invoking cancellation. Native catalogs override this callback to admit any name normalization before allocation.
    fn has_untyped_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let untyped = self.has_untyped_function(name);
        control.check()?;
        Ok(untyped)
    }

    /// Resolve a catalog-owned SQL type name that is not represented by the
    /// built-in [`ColumnType::from_sql_name`] mapping, such as a domain.
    fn resolve_type_name(&self, _name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }

    /// Resolve an explicit collation name with the binding catalog's namespace visibility and privileges.
    fn resolve_collation_name(&self, _name: &str) -> Result<Option<u32>, SQLError> {
        Ok(None)
    }

    /// Native catalogs admit namespace parsing and lookup scratch in this scope; the default adapts an external resolver's inline result.
    fn resolve_collation_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        control.check()?;
        let selected = self.resolve_collation_name(name)?;
        control.check()?;
        Ok(selected)
    }

    /// Enum labels of the binding catalog. Binding converts `unknown` literals coerced to an enum type with them, as `PostgreSQL` parse analysis calls the type's input function.
    fn enum_labels(&self) -> Option<&dyn crate::expr::enums::EnumLabelCatalog> {
        None
    }

    /// Input functions whose catalog constraints must run when an unknown literal is analyzed. Callers retain the returned constant; metadata-only inference does not invoke this capability.
    fn catalog_input_functions(&self) -> Option<&dyn crate::expr::CatalogInputFunctions> {
        None
    }

    /// Composite type attributes of the binding catalog, which type field selections and row coercions.
    fn composite_types(&self) -> Option<&dyn crate::expr::composites::CompositeTypeCatalog> {
        None
    }

    /// Require the current user's `USAGE` privilege on a type that a relation column, domain or routine declares, as `object_aclcheck(TypeRelationId, ..., ACL_USAGE)` and `aclcheck_error_type` require it; see [`crate::catalog::security::type_inquiry::usage_governing_type`]. A resolver without roles and type privileges has nothing to deny.
    fn require_type_usage(&self, _ty: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError>;

    fn resolve_function_type_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        control.check()?;
        production::external_type(
            self.resolve_function_type(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
            )?,
            control,
        )
    }

    fn resolve_function_overload_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        control.check()?;
        production::external_overload(
            self.resolve_function_overload(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
            )?,
            control,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the external resolver keeps its established call fields and adds the invoking production control"
    )]
    fn resolve_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        control.check()?;
        production::external_overload(
            self.resolve_function_overload_with_builtins(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                builtins,
            )?,
            control,
        )
    }

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

    /// Resolve the selected routine's execution category in the caller's production scope. Native catalog producers override this boundary; the default adapts an external resolver's boolean callback.
    fn is_scalar_function_binding_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let scalar = self.is_scalar_function_binding(binding)?;
        control.check()?;
        Ok(scalar)
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

    fn resolve_scalar_subquery_type_with_control(
        &self,
        subquery: crate::SubqueryId,
        outer_schema: &RowSchema,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        control.check()?;
        production::external_type(
            self.resolve_scalar_subquery_type(subquery, outer_schema, params)?,
            control,
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
    /// The final declared array parameter accepts expanded elements or an explicit VARIADIC array.
    pub variadic: bool,
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

/// Infer with the same catalog and observations while synthesized metadata retains its producer's allowance.
pub fn scalar_type_with_resolver_and_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    scalar_type_inner_with_control(expression, schema, params, resolver, control)
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

/// A bare string or NULL literal, which `PostgreSQL` types as `unknown` until its context resolves it.
pub fn is_unknown_literal(expression: &ScalarExpr) -> bool {
    matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null))
}

/// Whether a type's input function consults the catalog: the OID alias types and `aclitem` look names up, and an enum, a composite type, a record or a type known by name is read through its catalog definition. Analysis without the catalog leaves such a literal in the cast form that binding or evaluation reads, and reads every other literal with the type's input function.
pub fn catalog_input_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regcollation
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype
        | ColumnType::AclItem
        | ColumnType::PgNodeTree
        | ColumnType::Named(_)
        | ColumnType::Enum(_)
        | ColumnType::Composite(_)
        | ColumnType::Record
        | ColumnType::AnyArray
        | ColumnType::Vector(_)
        | ColumnType::Tensor(_) => true,
        ColumnType::Array(inner) | ColumnType::Domain { base: inner, .. } => {
            catalog_input_type(inner)
        }
        _ => false,
    }
}

/// Whether value operations can require live catalog metadata: enum identities, record fields and their array/domain containers cannot be evaluated by a catalog-free scalar implementation. OID aliases have already decoded their names and operate on ordinary integer values.
#[must_use]
pub fn catalog_value_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Named(_)
        | ColumnType::Enum(_)
        | ColumnType::Composite(_)
        | ColumnType::Record
        | ColumnType::AnyArray => true,
        ColumnType::Array(inner) | ColumnType::Domain { base: inner, .. } => {
            catalog_value_type(inner)
        }
        _ => false,
    }
}

/// The type of an assignment's source before coercion to its destination. A bare string or NULL literal has none: `transformAssignedExpr` and `transformAssignmentIndirection` convert it with the destination type's input function instead of checking a source type, so `'7'` assigns to an integer array element and `'abc'` fails as integer input.
pub fn assignment_source_type(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<ColumnType>, SQLError> {
    if is_unknown_literal(expression) {
        return Ok(None);
    }
    scalar_type_with_resolver(expression, schema, params, resolver)
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
pub use declaration::{resolve_alter_column_type, resolve_declared_column_type};

mod coercion;
pub mod composite_rows;
pub use coercion::coerce_common_context_value;
