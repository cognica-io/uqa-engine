//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`-compatible routine signature matching, including polymorphic and variadic parameters.

use super::overload_resolution::RankedFunctionMatch;
use crate::ast::{
    ColumnType, RoutineDefaultType, RoutineInvocationBinding,
    RoutineVariadicMode as InvocationVariadicMode,
};

/// One declared input or output parameter participating in routine-call matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineParameterDescriptor {
    pub name: Option<String>,
    pub type_name: String,
    /// Catalog-resolved declaration, including named domains and their base type.
    pub column_type: Option<ColumnType>,
    pub has_default: bool,
    /// Type of the analyzed default, considered only for a structurally omitted input.
    pub default_type: Option<RoutineDefaultType>,
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
    routine_polymorphic_type_with_control(
        type_name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary polymorphic type classification")
}

pub(crate) fn routine_polymorphic_type_with_control(
    type_name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<RoutinePolymorphicType>, uqa_core::ValueRetentionError> {
    let name =
        super::overload_resolution::canonical_routine_type_name_with_control(type_name, control)?;
    Ok(polymorphic_type_from_canonical(&name))
}

fn polymorphic_type_from_canonical(name: &str) -> Option<RoutinePolymorphicType> {
    Some(match name {
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
    InvalidVariadicSignature {
        reason: String,
    },
    InconsistentDefault {
        declared: String,
        first: Box<RoutineDefaultType>,
        second: Box<RoutineDefaultType>,
    },
    InconsistentPolymorphicTypes {
        first_declared: String,
        second_declared: String,
        first: Box<RoutineDefaultType>,
        second: Box<RoutineDefaultType>,
    },
    InvalidPolymorphicActual {
        declared: String,
        actual: RoutineDefaultType,
    },
    IncompatibleDefaultTypes {
        first: Box<ColumnType>,
        second: Box<ColumnType>,
    },
    IncompatibleRangeSubtype {
        declared: String,
        range: Box<ColumnType>,
        common: Box<ColumnType>,
    },
    InvalidPolymorphicElement {
        declared: String,
        actual: ColumnType,
    },
    IndeterminatePolymorphicArgument {
        declared: String,
    },
    IndeterminateArrayElement,
    IndeterminatePolymorphicType {
        family: RoutinePolymorphicFamily,
    },
}

impl RoutineSignatureMatchError {
    /// `PostgreSQL` SQLSTATE to use if candidate-set resolution determines that this error is final.
    #[must_use]
    pub const fn sqlstate(&self) -> &'static str {
        match self {
            Self::InvalidVariadicSignature { .. } => "42P13",
            _ => "42804",
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
        self.substitute_with_control(
            polymorphic,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary type substitution")
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary substituted type")
        })
    }
}

/// A fully matched call signature. Declared identity is kept separate from expansion-aware call binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRoutineSignature {
    pub declared_identity: Vec<String>,
    /// Declared types of supplied arguments, used for ranking before defaults are inserted.
    pub argument_signature: Vec<String>,
    /// Candidate-only failure reported after overload selection; never publish its coercion plan.
    pub default_error: Option<RoutineSignatureMatchError>,
    /// Effective target signature, one entry per supplied argument.
    pub argument_targets: Vec<String>,
    pub argument_sources: Vec<Option<String>>,
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
        self.substitute_type_with_control(
            declared_type_name,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary result type substitution")
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary substituted type")
        })
    }

    /// Resolve a return or output declaration to its canonical binding name.
    #[must_use]
    pub fn substitute_type_name(&self, declared_type_name: &str) -> Option<String> {
        self.substitute_type_name_with_control(
            declared_type_name,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary result type name")
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary substituted name")
        })
    }

    #[must_use]
    pub fn effective_argument_signature(&self) -> &[String] {
        &self.argument_signature
    }

    /// Materialize the stable AST binding contract consumed by scalar, FROM, CALL, and PL/pgSQL execution paths.
    #[must_use]
    pub fn invocation_binding(
        &self,
        declared_return_type: Option<&str>,
    ) -> RoutineInvocationBinding {
        self.invocation_binding_with_control(
            declared_return_type,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary invocation binding")
        .into_uncontrolled()
        .expect("ordinary invocation owner")
    }
}

impl RankedFunctionMatch for MatchedRoutineSignature {
    fn argument_types(&self) -> &[String] {
        &self.argument_signature
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

mod matching;
#[expect(
    clippy::result_large_err,
    reason = "owned diagnostics stay inline so failure propagation does not allocate another box after admission fails"
)]
mod production;
pub(crate) use production::{
    match_routine_candidate_with_control, RoutineInvocationParts, RoutineSignatureProductionError,
};

pub(crate) use matching::match_routine_candidate;
pub use matching::match_routine_signature;

#[cfg(test)]
mod tests;
