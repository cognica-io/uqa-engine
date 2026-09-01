//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`-compatible routine signature matching, including polymorphic and variadic parameters.

use super::overload_resolution::{
    canonical_column_type_name, canonical_routine_type_name, RankedFunctionMatch,
};
use uqa_sql::ast::{
    ColumnType, RoutineInvocationBinding, RoutineVariadicMode as InvocationVariadicMode,
};

/// One declared input or output parameter participating in routine-call matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineParameterDescriptor {
    pub name: Option<String>,
    pub type_name: String,
    pub has_default: bool,
    pub variadic: bool,
}

/// Call-site information that is independent from the routine's declared identity.
#[derive(Debug, Clone, Copy)]
pub struct RoutineCallDescriptor<'a> {
    pub argument_names: &'a [Option<String>],
    pub argument_types: &'a [Option<ColumnType>],
    pub explicit_variadic: bool,
}

/// `PostgreSQL`'s supported polymorphic pseudo-type spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutinePolymorphicType {
    AnyElement,
    AnyArray,
    AnyNonArray,
    AnyEnum,
    AnyRange,
    AnyMultirange,
    AnyCompatible,
    AnyCompatibleArray,
    AnyCompatibleNonArray,
    AnyCompatibleRange,
    AnyCompatibleMultirange,
}

impl RoutinePolymorphicType {
    /// Return whether the current [`ColumnType`] model can carry an actual value of this pseudo-type's constrained shape.
    #[must_use]
    pub const fn has_actual_carrier(self) -> bool {
        !matches!(self, Self::AnyEnum)
    }

    const fn family(self) -> RoutinePolymorphicFamily {
        match self {
            Self::AnyElement
            | Self::AnyArray
            | Self::AnyNonArray
            | Self::AnyEnum
            | Self::AnyRange
            | Self::AnyMultirange => RoutinePolymorphicFamily::Simple,
            Self::AnyCompatible
            | Self::AnyCompatibleArray
            | Self::AnyCompatibleNonArray
            | Self::AnyCompatibleRange
            | Self::AnyCompatibleMultirange => RoutinePolymorphicFamily::Compatible,
        }
    }

    const fn is_range_family(self) -> bool {
        matches!(
            self,
            Self::AnyRange
                | Self::AnyMultirange
                | Self::AnyCompatibleRange
                | Self::AnyCompatibleMultirange
        )
    }
}

/// Parse a routine pseudo-type after applying the same spelling normalization as routine identity matching.
#[must_use]
pub fn routine_polymorphic_type(type_name: &str) -> Option<RoutinePolymorphicType> {
    Some(match canonical_routine_type_name(type_name).as_str() {
        "anyelement" => RoutinePolymorphicType::AnyElement,
        "anyarray" => RoutinePolymorphicType::AnyArray,
        "anynonarray" => RoutinePolymorphicType::AnyNonArray,
        "anyenum" => RoutinePolymorphicType::AnyEnum,
        "anyrange" => RoutinePolymorphicType::AnyRange,
        "anymultirange" => RoutinePolymorphicType::AnyMultirange,
        "anycompatible" => RoutinePolymorphicType::AnyCompatible,
        "anycompatiblearray" => RoutinePolymorphicType::AnyCompatibleArray,
        "anycompatiblenonarray" => RoutinePolymorphicType::AnyCompatibleNonArray,
        "anycompatiblerange" => RoutinePolymorphicType::AnyCompatibleRange,
        "anycompatiblemultirange" => RoutinePolymorphicType::AnyCompatibleMultirange,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutinePolymorphicFamily {
    Simple,
    Compatible,
}

/// A structural or polymorphic-resolution error that must be distinguished from an ordinary non-matching candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineSignatureMatchError {
    InvalidVariadicSignature { reason: String },
    IndeterminatePolymorphicType { family: RoutinePolymorphicFamily },
}

impl RoutineSignatureMatchError {
    /// `PostgreSQL` SQLSTATE to use if candidate-set resolution determines that this error is final.
    #[must_use]
    pub const fn sqlstate(&self) -> &'static str {
        match self {
            Self::InvalidVariadicSignature { .. } => "42P13",
            Self::IndeterminatePolymorphicType { .. } => "42804",
        }
    }
}

/// One supplied argument's concrete coercion destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineCoercionTarget {
    pub type_name: String,
    /// Named catalog types that are not represented by [`ColumnType`] retain their canonical name while leaving this carrier absent.
    pub column_type: Option<ColumnType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineVariadicMode {
    None,
    Pack,
    PassThrough,
    Default,
}

/// Execution plan for the declared variadic array parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineVariadicPlan {
    None,
    Pack {
        parameter_index: usize,
        argument_indices: Vec<usize>,
        element_type: String,
        array_type: String,
    },
    PassThrough {
        parameter_index: usize,
        argument_index: usize,
        array_type: String,
    },
    Default {
        parameter_index: usize,
        array_type: String,
    },
}

/// Concrete substitutions shared by input, return, and output pseudo-types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutineTypeSubstitutions {
    pub simple_element: Option<ColumnType>,
    pub simple_array: Option<ColumnType>,
    pub simple_range: Option<ColumnType>,
    pub simple_multirange: Option<ColumnType>,
    pub compatible_element: Option<ColumnType>,
    pub compatible_array: Option<ColumnType>,
    pub compatible_range: Option<ColumnType>,
    pub compatible_multirange: Option<ColumnType>,
}

impl RoutineTypeSubstitutions {
    #[must_use]
    pub fn substitute(&self, polymorphic: RoutinePolymorphicType) -> Option<ColumnType> {
        match polymorphic {
            RoutinePolymorphicType::AnyElement | RoutinePolymorphicType::AnyNonArray => {
                self.simple_element.clone()
            }
            RoutinePolymorphicType::AnyArray => self.simple_array.clone(),
            RoutinePolymorphicType::AnyRange => self.simple_range.clone(),
            RoutinePolymorphicType::AnyMultirange => self.simple_multirange.clone(),
            RoutinePolymorphicType::AnyCompatible
            | RoutinePolymorphicType::AnyCompatibleNonArray => self.compatible_element.clone(),
            RoutinePolymorphicType::AnyCompatibleArray => self.compatible_array.clone(),
            RoutinePolymorphicType::AnyCompatibleRange => self.compatible_range.clone(),
            RoutinePolymorphicType::AnyCompatibleMultirange => self.compatible_multirange.clone(),
            RoutinePolymorphicType::AnyEnum => None,
        }
    }
}

/// A fully matched call signature. Declared identity is kept separate from expansion-aware call binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRoutineSignature {
    pub declared_identity: Vec<String>,
    /// Effective target signature, one entry per supplied argument.
    pub argument_targets: Vec<String>,
    /// Supplied-argument to declared-parameter mapping; an expanded variadic position may occur repeatedly.
    pub argument_positions: Vec<usize>,
    pub coercion_targets: Vec<RoutineCoercionTarget>,
    /// Concrete parameter types aligned with the declared parameter vector.
    pub parameter_types: Vec<String>,
    pub parameter_type_values: Vec<Option<ColumnType>>,
    pub defaulted_parameters: Vec<usize>,
    pub variadic_mode: RoutineVariadicMode,
    pub variadic_plan: RoutineVariadicPlan,
    pub substitutions: RoutineTypeSubstitutions,
    pub raw_exact_matches: usize,
    pub exact_matches: usize,
    pub preferred_matches: usize,
}

impl MatchedRoutineSignature {
    /// Resolve a declared return or output type through this call's polymorphic substitutions.
    #[must_use]
    pub fn substitute_type(&self, declared_type_name: &str) -> Option<ColumnType> {
        if let Some(polymorphic) = routine_polymorphic_type(declared_type_name) {
            return self.substitutions.substitute(polymorphic);
        }
        let canonical = canonical_routine_type_name(declared_type_name);
        ColumnType::from_sql_name(&canonical).ok().or_else(|| {
            self.declared_identity
                .iter()
                .zip(&self.parameter_type_values)
                .find_map(|(declared, resolved)| {
                    (declared == &canonical).then(|| resolved.clone()).flatten()
                })
        })
    }

    /// Resolve a declared return or output type to its canonical binding name even when no [`ColumnType`] carrier exists for a named catalog type.
    #[must_use]
    pub fn substitute_type_name(&self, declared_type_name: &str) -> Option<String> {
        if routine_polymorphic_type(declared_type_name).is_some() {
            return self
                .substitute_type(declared_type_name)
                .map(|ty| canonical_column_type_name(&ty));
        }
        Some(canonical_routine_type_name(declared_type_name))
    }

    #[must_use]
    pub fn effective_argument_signature(&self) -> &[String] {
        &self.argument_targets
    }

    /// Materialize the stable AST binding contract consumed by scalar, FROM, CALL, and PL/pgSQL execution paths.
    #[must_use]
    pub fn invocation_binding(
        &self,
        declared_return_type: Option<&str>,
    ) -> RoutineInvocationBinding {
        let variadic_mode = match (&self.variadic_plan, self.variadic_mode) {
            (
                RoutineVariadicPlan::Pack {
                    parameter_index, ..
                },
                RoutineVariadicMode::Pack,
            ) => InvocationVariadicMode::Expanded {
                parameter_index: *parameter_index,
            },
            (
                RoutineVariadicPlan::PassThrough {
                    parameter_index, ..
                },
                RoutineVariadicMode::PassThrough,
            ) => InvocationVariadicMode::Explicit {
                parameter_index: *parameter_index,
            },
            _ => InvocationVariadicMode::None,
        };
        RoutineInvocationBinding {
            argument_positions: self.argument_positions.clone(),
            argument_targets: self.argument_targets.clone(),
            parameter_types: self.parameter_types.clone(),
            return_type: declared_return_type
                .and_then(|type_name| self.substitute_type_name(type_name)),
            variadic_mode,
        }
    }
}

impl RankedFunctionMatch for MatchedRoutineSignature {
    fn argument_types(&self) -> &[String] {
        &self.argument_targets
    }

    fn raw_exact_matches(&self) -> usize {
        self.raw_exact_matches
    }

    fn exact_matches(&self) -> usize {
        self.exact_matches
    }

    fn preferred_matches(&self) -> usize {
        self.preferred_matches
    }

    fn is_variadic_expansion(&self) -> bool {
        self.variadic_mode == RoutineVariadicMode::Pack
    }
}

mod mapping;
mod matching;
mod polymorphic;

pub use matching::match_routine_signature;

#[cfg(test)]
mod tests;
