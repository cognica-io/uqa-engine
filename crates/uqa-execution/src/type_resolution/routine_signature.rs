//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible routine signature matching, including polymorphic and variadic parameters.

use uqa_sql::ast::{
    ColumnType, RoutineInvocationBinding, RoutineVariadicMode as InvocationVariadicMode,
};

use super::common::{base_type, common_type};
use super::overload_resolution::{
    canonical_column_type_name, canonical_routine_type_name, routine_type_accepts_implicit_cast,
    routine_type_is_preferred, RankedFunctionMatch,
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
        !matches!(
            self,
            Self::AnyEnum
                | Self::AnyRange
                | Self::AnyMultirange
                | Self::AnyCompatibleRange
                | Self::AnyCompatibleMultirange
        )
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

    const fn is_unsupported_range(self) -> bool {
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
    pub compatible_element: Option<ColumnType>,
    pub compatible_array: Option<ColumnType>,
}

impl RoutineTypeSubstitutions {
    #[must_use]
    pub fn substitute(&self, polymorphic: RoutinePolymorphicType) -> Option<ColumnType> {
        match polymorphic {
            RoutinePolymorphicType::AnyElement | RoutinePolymorphicType::AnyNonArray => {
                self.simple_element.clone()
            }
            RoutinePolymorphicType::AnyArray => self.simple_array.clone(),
            RoutinePolymorphicType::AnyCompatible
            | RoutinePolymorphicType::AnyCompatibleNonArray => self.compatible_element.clone(),
            RoutinePolymorphicType::AnyCompatibleArray => self.compatible_array.clone(),
            RoutinePolymorphicType::AnyEnum
            | RoutinePolymorphicType::AnyRange
            | RoutinePolymorphicType::AnyMultirange
            | RoutinePolymorphicType::AnyCompatibleRange
            | RoutinePolymorphicType::AnyCompatibleMultirange => None,
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

#[derive(Debug)]
struct StructuralMapping {
    argument_positions: Vec<usize>,
    slots: Vec<Vec<usize>>,
    defaulted_parameters: Vec<usize>,
    variadic_index: Option<usize>,
    variadic_mode: RoutineVariadicMode,
}

/// Match one declared routine signature without changing its catalog identity.
pub fn match_routine_signature(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let Some(mapping) = structural_mapping(parameters, call)? else {
        return Ok(None);
    };

    let mut substitutions = RoutineTypeSubstitutions::default();
    let mut simple_used = false;
    let mut compatible_used = false;
    for (parameter_index, parameter) in parameters.iter().enumerate() {
        let Some(polymorphic) = routine_polymorphic_type(&parameter.type_name) else {
            continue;
        };
        if !polymorphic.has_actual_carrier() {
            if polymorphic.is_unsupported_range()
                && mapping.slots[parameter_index]
                    .iter()
                    .any(|argument_index| call.argument_types[*argument_index].is_none())
            {
                return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                    family: polymorphic.family(),
                });
            }
            return Ok(None);
        }
        match polymorphic.family() {
            RoutinePolymorphicFamily::Simple => simple_used = true,
            RoutinePolymorphicFamily::Compatible => compatible_used = true,
        }
    }

    let mut simple_element: Option<ColumnType> = None;
    let mut simple_array: Option<ColumnType> = None;
    let mut compatible_element: Option<ColumnType> = None;
    for (argument_index, parameter_index) in mapping.argument_positions.iter().copied().enumerate()
    {
        let effective_declared = effective_declared_type(
            &parameters[parameter_index],
            Some(parameter_index) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack,
        );
        let Some(polymorphic) = routine_polymorphic_type(&effective_declared) else {
            continue;
        };
        let Some(actual) = call.argument_types[argument_index].as_ref() else {
            if !polymorphic.has_actual_carrier() && polymorphic.is_unsupported_range() {
                return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                    family: polymorphic.family(),
                });
            }
            if !polymorphic.has_actual_carrier() {
                return Ok(None);
            }
            continue;
        };
        if !collect_polymorphic_actual(
            polymorphic,
            actual,
            &mut simple_element,
            &mut simple_array,
            &mut compatible_element,
        ) {
            return Ok(None);
        }
    }

    if simple_used {
        let Some(element) = simple_element else {
            return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                family: RoutinePolymorphicFamily::Simple,
            });
        };
        substitutions.simple_array =
            Some(simple_array.unwrap_or_else(|| ColumnType::Array(Box::new(element.clone()))));
        substitutions.simple_element = Some(element);
    }
    if compatible_used {
        let element = compatible_element.unwrap_or(ColumnType::Text);
        substitutions.compatible_array = Some(ColumnType::Array(Box::new(element.clone())));
        substitutions.compatible_element = Some(element);
    }

    let mut argument_targets = Vec::with_capacity(call.argument_types.len());
    let mut coercion_targets = Vec::with_capacity(call.argument_types.len());
    let mut raw_exact_matches = 0usize;
    let mut exact_matches = 0usize;
    let mut preferred_matches = 0usize;
    for (argument_index, parameter_index) in mapping.argument_positions.iter().copied().enumerate()
    {
        let effective_declared = effective_declared_type(
            &parameters[parameter_index],
            Some(parameter_index) == mapping.variadic_index
                && mapping.variadic_mode == RoutineVariadicMode::Pack,
        );
        let actual = call.argument_types[argument_index].as_ref();
        let Some(target) = resolve_target(&effective_declared, actual, &substitutions) else {
            return Ok(None);
        };
        if let Some(actual) = actual {
            if routine_polymorphic_type(&effective_declared).is_none() {
                let raw_actual = canonical_column_type_name(actual);
                let base_actual = canonical_column_type_name(base_type(actual));
                if raw_actual == target.type_name {
                    raw_exact_matches += 1;
                    exact_matches += 1;
                } else if base_actual == target.type_name {
                    exact_matches += 1;
                } else if routine_type_accepts_implicit_cast(&base_actual, &target.type_name) {
                    preferred_matches += usize::from(routine_type_is_preferred(&target.type_name));
                } else {
                    return Ok(None);
                }
            } else if !actual_accepts_polymorphic_target(actual, &target) {
                return Ok(None);
            }
        }
        argument_targets.push(target.type_name.clone());
        coercion_targets.push(target);
    }

    let declared_identity = parameters
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect::<Vec<_>>();
    let mut parameter_types = Vec::with_capacity(parameters.len());
    let mut parameter_type_values = Vec::with_capacity(parameters.len());
    for (parameter_index, parameter) in parameters.iter().enumerate() {
        let actual = mapping.slots[parameter_index]
            .first()
            .and_then(|argument_index| call.argument_types[*argument_index].as_ref());
        let Some(target) = resolve_target(&parameter.type_name, actual, &substitutions) else {
            return Ok(None);
        };
        parameter_types.push(target.type_name);
        parameter_type_values.push(target.column_type);
    }

    let variadic_plan = match (mapping.variadic_index, mapping.variadic_mode) {
        (None, RoutineVariadicMode::None) => RoutineVariadicPlan::None,
        (Some(parameter_index), RoutineVariadicMode::Pack) => RoutineVariadicPlan::Pack {
            parameter_index,
            argument_indices: mapping.slots[parameter_index].clone(),
            element_type: mapping.slots[parameter_index]
                .first()
                .map(|argument_index| argument_targets[*argument_index].clone())
                .expect("a variadic pack has at least one argument"),
            array_type: parameter_types[parameter_index].clone(),
        },
        (Some(parameter_index), RoutineVariadicMode::PassThrough) => {
            RoutineVariadicPlan::PassThrough {
                parameter_index,
                argument_index: *mapping.slots[parameter_index]
                    .first()
                    .expect("an explicit variadic call has one array argument"),
                array_type: parameter_types[parameter_index].clone(),
            }
        }
        (Some(parameter_index), RoutineVariadicMode::Default) => RoutineVariadicPlan::Default {
            parameter_index,
            array_type: parameter_types[parameter_index].clone(),
        },
        _ => unreachable!("structural mapping returned an inconsistent variadic mode"),
    };

    Ok(Some(MatchedRoutineSignature {
        declared_identity,
        argument_targets,
        argument_positions: mapping.argument_positions,
        coercion_targets,
        parameter_types,
        parameter_type_values,
        defaulted_parameters: mapping.defaulted_parameters,
        variadic_mode: mapping.variadic_mode,
        variadic_plan,
        substitutions,
        raw_exact_matches,
        exact_matches,
        preferred_matches,
    }))
}

fn structural_mapping(
    parameters: &[RoutineParameterDescriptor],
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<StructuralMapping>, RoutineSignatureMatchError> {
    if call.argument_names.len() != call.argument_types.len() {
        return Ok(None);
    }
    let variadic_indices = parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.variadic.then_some(index))
        .collect::<Vec<_>>();
    if variadic_indices.len() > 1
        || variadic_indices
            .first()
            .is_some_and(|index| *index + 1 != parameters.len())
    {
        return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
            reason: "VARIADIC parameter must be the final and only variadic parameter".into(),
        });
    }
    let variadic_index = variadic_indices.first().copied();
    if variadic_index.is_none()
        && call.explicit_variadic
        && call.argument_names.iter().any(Option::is_some)
    {
        return Ok(None);
    }
    // PostgreSQL excludes an expanded variadic candidate whenever named notation is used, even when every omitted variadic position has a default. An explicit `VARIADIC name => array` call still targets the declared array slot, and the marker does not exclude an otherwise matching fixed-array candidate.
    if variadic_index.is_some()
        && !call.explicit_variadic
        && call.argument_names.iter().any(Option::is_some)
    {
        return Ok(None);
    }
    if let Some(index) = variadic_index {
        if !is_array_declaration(&parameters[index].type_name) {
            return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
                reason: "VARIADIC parameter must have an array type".into(),
            });
        }
        if !parameters[index].has_default
            && parameters[..index]
                .iter()
                .any(|parameter| parameter.has_default)
        {
            return Err(RoutineSignatureMatchError::InvalidVariadicSignature {
                reason: "input parameters after one with a default value must also have defaults"
                    .into(),
            });
        }
    } else if call.argument_types.len() > parameters.len() {
        return Ok(None);
    }

    let mut argument_positions = vec![usize::MAX; call.argument_types.len()];
    let mut reserved = vec![false; parameters.len()];
    let mut saw_named = false;
    for (argument_index, argument_name) in call.argument_names.iter().enumerate() {
        if let Some(argument_name) = argument_name {
            saw_named = true;
            let Some(parameter_index) = parameters
                .iter()
                .position(|parameter| parameter.name.as_deref() == Some(argument_name.as_str()))
            else {
                return Ok(None);
            };
            if reserved[parameter_index]
                || Some(parameter_index) == variadic_index && !call.explicit_variadic
            {
                return Ok(None);
            }
            reserved[parameter_index] = true;
            argument_positions[argument_index] = parameter_index;
        } else if saw_named {
            return Ok(None);
        }
    }

    let positional_count = call
        .argument_names
        .iter()
        .take_while(|name| name.is_none())
        .count();
    let mut parameter_index = 0usize;
    for (argument_index, argument_position) in argument_positions
        .iter_mut()
        .take(positional_count)
        .enumerate()
    {
        let remaining_arguments = positional_count - argument_index;
        loop {
            if parameter_index >= parameters.len() {
                if let Some(variadic_index) = variadic_index.filter(|_| !call.explicit_variadic) {
                    *argument_position = variadic_index;
                    break;
                }
                return Ok(None);
            }
            if reserved[parameter_index] {
                return Ok(None);
            }
            if Some(parameter_index) == variadic_index {
                if call.explicit_variadic {
                    if remaining_arguments != 1 {
                        return Ok(None);
                    }
                    *argument_position = parameter_index;
                    parameter_index += 1;
                } else {
                    *argument_position = parameter_index;
                }
                break;
            }
            let required_remaining = parameters[parameter_index..]
                .iter()
                .enumerate()
                .filter(|(offset, parameter)| {
                    let index = parameter_index + offset;
                    !reserved[index] && !parameter.has_default
                })
                .count();
            if parameters[parameter_index].has_default && remaining_arguments == required_remaining
            {
                parameter_index += 1;
                continue;
            }
            *argument_position = parameter_index;
            parameter_index += 1;
            break;
        }
    }

    if argument_positions.contains(&usize::MAX) {
        return Ok(None);
    }
    let mut slots = vec![Vec::new(); parameters.len()];
    for (argument_index, parameter_index) in argument_positions.iter().copied().enumerate() {
        if !slots[parameter_index].is_empty()
            && (Some(parameter_index) != variadic_index || call.explicit_variadic)
        {
            return Ok(None);
        }
        slots[parameter_index].push(argument_index);
    }

    let mut defaulted_parameters = Vec::new();
    for (index, (parameter, slot)) in parameters.iter().zip(&slots).enumerate() {
        if slot.is_empty() {
            if !parameter.has_default {
                return Ok(None);
            }
            defaulted_parameters.push(index);
        }
    }
    let variadic_mode = match variadic_index {
        None => RoutineVariadicMode::None,
        Some(index) if call.explicit_variadic => {
            if slots[index].len() != 1 {
                return Ok(None);
            }
            RoutineVariadicMode::PassThrough
        }
        Some(index) if slots[index].is_empty() => RoutineVariadicMode::Default,
        Some(_) => RoutineVariadicMode::Pack,
    };
    Ok(Some(StructuralMapping {
        argument_positions,
        slots,
        defaulted_parameters,
        variadic_index,
        variadic_mode,
    }))
}

fn is_array_declaration(type_name: &str) -> bool {
    let canonical = canonical_routine_type_name(type_name);
    canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
}

fn effective_declared_type(parameter: &RoutineParameterDescriptor, expanded: bool) -> String {
    let canonical = canonical_routine_type_name(&parameter.type_name);
    if !expanded {
        return canonical;
    }
    match canonical.as_str() {
        "anyarray" => "anyelement".into(),
        "anycompatiblearray" => "anycompatible".into(),
        "int2vector" => "int2".into(),
        "oidvector" => "oid".into(),
        _ => canonical
            .strip_suffix("[]")
            .expect("validated variadic array declaration")
            .into(),
    }
}

fn collect_polymorphic_actual(
    polymorphic: RoutinePolymorphicType,
    actual: &ColumnType,
    simple_element: &mut Option<ColumnType>,
    simple_array: &mut Option<ColumnType>,
    compatible_element: &mut Option<ColumnType>,
) -> bool {
    match polymorphic {
        RoutinePolymorphicType::AnyElement => {
            merge_same_identity(simple_element, normalize_identity_type(actual))
        }
        RoutinePolymorphicType::AnyNonArray => {
            !is_array_actual(actual)
                && merge_same_identity(simple_element, normalize_identity_type(actual))
        }
        RoutinePolymorphicType::AnyArray => {
            let Some((element, array)) = array_actual(actual) else {
                return false;
            };
            merge_same_identity(simple_element, element) && merge_same_identity(simple_array, array)
        }
        RoutinePolymorphicType::AnyCompatible => {
            merge_compatible(compatible_element, normalize_identity_type(actual))
        }
        RoutinePolymorphicType::AnyCompatibleNonArray => {
            !is_array_actual(actual)
                && merge_compatible(compatible_element, normalize_identity_type(actual))
        }
        RoutinePolymorphicType::AnyCompatibleArray => {
            let Some((element, _)) = array_actual(actual) else {
                return false;
            };
            merge_compatible(compatible_element, element)
        }
        RoutinePolymorphicType::AnyEnum
        | RoutinePolymorphicType::AnyRange
        | RoutinePolymorphicType::AnyMultirange
        | RoutinePolymorphicType::AnyCompatibleRange
        | RoutinePolymorphicType::AnyCompatibleMultirange => false,
    }
}

fn merge_same_identity(slot: &mut Option<ColumnType>, candidate: ColumnType) -> bool {
    if let Some(current) = slot {
        canonical_column_type_name(current) == canonical_column_type_name(&candidate)
    } else {
        *slot = Some(candidate);
        true
    }
}

fn merge_compatible(slot: &mut Option<ColumnType>, candidate: ColumnType) -> bool {
    match slot.take() {
        None => *slot = Some(candidate),
        Some(current) => match common_type(&current, &candidate) {
            Ok(common) => *slot = Some(normalize_identity_type(&common)),
            Err(_) => {
                *slot = Some(current);
                return false;
            }
        },
    }
    true
}

fn normalize_identity_type(actual: &ColumnType) -> ColumnType {
    if matches!(actual, ColumnType::Domain { .. }) {
        return actual.clone();
    }
    ColumnType::from_sql_name(&canonical_column_type_name(actual))
        .unwrap_or_else(|_| actual.clone())
}

fn array_actual(actual: &ColumnType) -> Option<(ColumnType, ColumnType)> {
    let actual = base_type(actual);
    match actual {
        ColumnType::Array(element) => Some((
            normalize_identity_type(element),
            normalize_identity_type(actual),
        )),
        ColumnType::Int2Vector => Some((ColumnType::SmallInteger, ColumnType::Int2Vector)),
        ColumnType::OidVector => Some((ColumnType::Oid, ColumnType::OidVector)),
        _ => None,
    }
}

fn is_array_actual(actual: &ColumnType) -> bool {
    array_actual(actual).is_some() || matches!(base_type(actual), ColumnType::AnyArray)
}

fn resolve_target(
    declared_type_name: &str,
    actual: Option<&ColumnType>,
    substitutions: &RoutineTypeSubstitutions,
) -> Option<RoutineCoercionTarget> {
    if let Some(polymorphic) = routine_polymorphic_type(declared_type_name) {
        let column_type = substitutions.substitute(polymorphic)?;
        return Some(RoutineCoercionTarget {
            type_name: canonical_column_type_name(&column_type),
            column_type: Some(column_type),
        });
    }
    let type_name = canonical_routine_type_name(declared_type_name);
    let column_type = actual
        .filter(|actual| canonical_column_type_name(actual) == type_name)
        .cloned()
        .or_else(|| {
            actual
                .map(base_type)
                .filter(|actual| canonical_column_type_name(actual) == type_name)
                .cloned()
        })
        .or_else(|| ColumnType::from_sql_name(&type_name).ok());
    Some(RoutineCoercionTarget {
        type_name,
        column_type,
    })
}

fn actual_accepts_polymorphic_target(actual: &ColumnType, target: &RoutineCoercionTarget) -> bool {
    let raw_actual = canonical_column_type_name(actual);
    let base_actual = canonical_column_type_name(base_type(actual));
    raw_actual == target.type_name
        || base_actual == target.type_name
        || routine_type_accepts_implicit_cast(&base_actual, &target.type_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_resolution::rank_function_matches;

    fn parameter(type_name: &str) -> RoutineParameterDescriptor {
        RoutineParameterDescriptor {
            name: None,
            type_name: type_name.into(),
            has_default: false,
            variadic: false,
        }
    }

    fn match_types(
        parameters: &[RoutineParameterDescriptor],
        argument_types: &[Option<ColumnType>],
    ) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
        let argument_names = vec![None; argument_types.len()];
        match_routine_signature(
            parameters,
            RoutineCallDescriptor {
                argument_names: &argument_names,
                argument_types,
                explicit_variadic: false,
            },
        )
    }

    #[test]
    fn parses_every_polymorphic_family_and_marks_missing_actual_carriers() {
        let cases = [
            ("anyelement", true),
            ("anyarray", true),
            ("anynonarray", true),
            ("anyenum", false),
            ("anyrange", false),
            ("anymultirange", false),
            ("anycompatible", true),
            ("anycompatiblearray", true),
            ("anycompatiblenonarray", true),
            ("anycompatiblerange", false),
            ("anycompatiblemultirange", false),
        ];
        for (type_name, has_actual_carrier) in cases {
            assert_eq!(
                routine_polymorphic_type(type_name)
                    .expect("known polymorphic spelling")
                    .has_actual_carrier(),
                has_actual_carrier,
                "{type_name}"
            );
        }
    }

    #[test]
    fn simple_family_requires_known_consistent_types_and_substitutes_outputs() {
        let matched = match_types(&[parameter("anyelement")], &[Some(ColumnType::Integer)])
            .unwrap()
            .unwrap();
        assert_eq!(matched.argument_positions, [0]);
        assert_eq!(matched.argument_targets, ["int4"]);
        assert_eq!(matched.parameter_types, ["int4"]);
        assert_eq!(
            matched.substitute_type("anyelement"),
            Some(ColumnType::Integer)
        );
        assert_eq!(
            matched.substitute_type("anyarray"),
            Some(ColumnType::Array(Box::new(ColumnType::Integer)))
        );

        let error = match_types(&[parameter("anyelement")], &[None]).unwrap_err();
        assert_eq!(error.sqlstate(), "42804");

        assert!(match_types(
            &[parameter("anyarray"), parameter("anyelement")],
            &[
                Some(ColumnType::Array(Box::new(ColumnType::Integer))),
                Some(ColumnType::BigInteger),
            ],
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn simple_anyarray_flattens_array_domains() {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "ints".into(),
            oid: 42,
            base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
        };
        let matched = match_types(&[parameter("anyarray")], &[Some(domain)])
            .unwrap()
            .unwrap();
        assert_eq!(matched.argument_targets, ["int4[]"]);
        assert_eq!(
            matched.substitute_type("anyelement"),
            Some(ColumnType::Integer)
        );
    }

    #[test]
    fn compatible_family_uses_common_type_and_unknown_text_fallback() {
        let numeric = ColumnType::Numeric {
            precision: None,
            scale: None,
        };
        let cases = [
            (ColumnType::SmallInteger, ColumnType::Integer, "int4"),
            (ColumnType::Integer, ColumnType::BigInteger, "int8"),
            (ColumnType::Integer, numeric, "numeric"),
        ];
        for (left, right, expected) in cases {
            let matched = match_types(
                &[parameter("anycompatible"), parameter("anycompatible")],
                &[Some(left), Some(right)],
            )
            .unwrap()
            .unwrap();
            assert_eq!(matched.argument_targets, [expected, expected]);
        }
        let matched = match_types(
            &[parameter("anycompatible"), parameter("anycompatible")],
            &[None, None],
        )
        .unwrap()
        .unwrap();
        assert_eq!(matched.argument_targets, ["text", "text"]);

        assert!(match_types(
            &[parameter("anycompatiblenonarray")],
            &[Some(ColumnType::Array(Box::new(ColumnType::Integer)))],
        )
        .unwrap()
        .is_none());

        let array = match_types(
            &[parameter("anycompatiblearray"), parameter("anycompatible")],
            &[
                Some(ColumnType::Array(Box::new(ColumnType::Integer))),
                Some(ColumnType::BigInteger),
            ],
        )
        .unwrap()
        .unwrap();
        assert_eq!(array.argument_targets, ["int8[]", "int8"]);
    }

    #[test]
    fn unavailable_enum_and_range_carriers_do_not_claim_concrete_actuals() {
        for type_name in [
            "anyenum",
            "anyrange",
            "anymultirange",
            "anycompatiblerange",
            "anycompatiblemultirange",
        ] {
            assert!(
                match_types(&[parameter(type_name)], &[Some(ColumnType::Integer)])
                    .unwrap()
                    .is_none()
            );
        }
        for type_name in [
            "anyrange",
            "anymultirange",
            "anycompatiblerange",
            "anycompatiblemultirange",
        ] {
            assert_eq!(
                match_types(&[parameter(type_name)], &[None])
                    .unwrap_err()
                    .sqlstate(),
                "42804"
            );
        }
        assert!(match_types(&[parameter("anyenum")], &[None])
            .unwrap()
            .is_none());
    }

    #[test]
    fn compatible_family_preserves_equal_domains_and_flattens_mixed_domains() {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "positive_int".into(),
            oid: 43,
            base: Box::new(ColumnType::Integer),
        };
        let same = match_types(
            &[parameter("anycompatible"), parameter("anycompatible")],
            &[Some(domain.clone()), Some(domain.clone())],
        )
        .unwrap()
        .unwrap();
        assert_eq!(same.argument_targets, ["public.positive_int"; 2]);

        let mixed = match_types(
            &[parameter("anycompatible"), parameter("anycompatible")],
            &[Some(domain), Some(ColumnType::Integer)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(mixed.argument_targets, ["int4", "int4"]);
    }

    #[test]
    fn variadic_mapping_distinguishes_pack_default_and_explicit_array() {
        let mut variadic = parameter("int4[]");
        variadic.name = Some("xs".into());
        variadic.variadic = true;

        assert!(match_types(&[variadic.clone()], &[]).unwrap().is_none());
        let packed = match_types(
            &[variadic.clone()],
            &[Some(ColumnType::Integer), Some(ColumnType::SmallInteger)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(packed.argument_positions, [0, 0]);
        assert_eq!(packed.argument_targets, ["int4", "int4"]);
        assert_eq!(packed.variadic_mode, RoutineVariadicMode::Pack);

        variadic.has_default = true;
        let defaulted = match_types(&[variadic.clone()], &[]).unwrap().unwrap();
        assert_eq!(defaulted.variadic_mode, RoutineVariadicMode::Default);
        assert_eq!(defaulted.defaulted_parameters, [0]);

        let argument_names = [None];
        let argument_types = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
        let passed = match_routine_signature(
            &[variadic.clone()],
            RoutineCallDescriptor {
                argument_names: &argument_names,
                argument_types: &argument_types,
                explicit_variadic: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(passed.argument_targets, ["int4[]"]);
        assert_eq!(passed.variadic_mode, RoutineVariadicMode::PassThrough);

        let named = [Some("xs".into())];
        assert!(match_routine_signature(
            &[variadic.clone()],
            RoutineCallDescriptor {
                argument_names: &named,
                argument_types: &argument_types,
                explicit_variadic: false,
            },
        )
        .unwrap()
        .is_none());
        let explicit_named = match_routine_signature(
            &[variadic],
            RoutineCallDescriptor {
                argument_names: &named,
                argument_types: &argument_types,
                explicit_variadic: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            explicit_named.variadic_mode,
            RoutineVariadicMode::PassThrough
        );
    }

    #[test]
    fn variadic_candidates_reject_named_notation_but_keep_positional_defaults() {
        let mut first = parameter("int4");
        first.name = Some("a".into());
        let mut second = parameter("int4");
        second.name = Some("b".into());
        second.has_default = true;
        let mut variadic = parameter("int4[]");
        variadic.name = Some("xs".into());
        variadic.has_default = true;
        variadic.variadic = true;
        let parameters = [first, second, variadic];

        let positional_names = [None];
        let positional_types = [Some(ColumnType::Integer)];
        let positional = match_routine_signature(
            &parameters,
            RoutineCallDescriptor {
                argument_names: &positional_names,
                argument_types: &positional_types,
                explicit_variadic: false,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(positional.defaulted_parameters, [1, 2]);
        assert_eq!(positional.variadic_mode, RoutineVariadicMode::Default);

        let named = [Some("a".into())];
        assert!(match_routine_signature(
            &parameters,
            RoutineCallDescriptor {
                argument_names: &named,
                argument_types: &positional_types,
                explicit_variadic: false,
            },
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn generic_variadic_infers_element_and_concrete_parameter_array() {
        let mut variadic = parameter("anycompatiblearray");
        variadic.variadic = true;
        let matched = match_types(&[variadic], &[Some(ColumnType::Integer), None])
            .unwrap()
            .unwrap();
        assert_eq!(matched.argument_targets, ["int4", "int4"]);
        assert_eq!(matched.parameter_types, ["int4[]"]);
        assert_eq!(
            matched.substitute_type_name("anycompatiblearray"),
            Some("int4[]".into())
        );
        let invocation = matched.invocation_binding(Some("anycompatible"));
        assert_eq!(invocation.argument_positions, [0, 0]);
        assert_eq!(invocation.argument_targets, ["int4", "int4"]);
        assert_eq!(invocation.parameter_types, ["int4[]"]);
        assert_eq!(invocation.return_type, Some("int4".into()));
        assert_eq!(
            invocation.variadic_mode,
            InvocationVariadicMode::Expanded { parameter_index: 0 }
        );
    }

    #[test]
    fn ranking_keeps_oracle_ambiguities_and_prefers_fixed_equivalent_signature() {
        let array_actual = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
        let anyelement = match_types(&[parameter("anyelement")], &array_actual)
            .unwrap()
            .unwrap();
        let anyarray = match_types(&[parameter("anyarray")], &array_actual)
            .unwrap()
            .unwrap();
        let mut ambiguous = vec![anyelement, anyarray];
        assert!(rank_function_matches(&mut ambiguous, &array_actual));
        assert_eq!(ambiguous.len(), 2);

        let fixed = match_types(&[parameter("int4")], &[Some(ColumnType::Integer)])
            .unwrap()
            .unwrap();
        let mut variadic_parameter = parameter("int4[]");
        variadic_parameter.variadic = true;
        let expanded = match_types(&[variadic_parameter], &[Some(ColumnType::Integer)])
            .unwrap()
            .unwrap();
        let mut fixed_over_variadic = vec![expanded, fixed];
        assert!(rank_function_matches(
            &mut fixed_over_variadic,
            &[Some(ColumnType::Integer)]
        ));
        assert_eq!(fixed_over_variadic.len(), 1);
        assert_eq!(
            fixed_over_variadic[0].variadic_mode,
            RoutineVariadicMode::None
        );
    }

    #[test]
    fn fixed_array_candidate_survives_explicit_variadic_marker() {
        let argument_names = [None];
        let argument_types = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
        let fixed = match_routine_signature(
            &[parameter("int4[]")],
            RoutineCallDescriptor {
                argument_names: &argument_names,
                argument_types: &argument_types,
                explicit_variadic: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(fixed.variadic_mode, RoutineVariadicMode::None);

        let named = [Some("value".into())];
        assert!(match_routine_signature(
            &[parameter("int4[]")],
            RoutineCallDescriptor {
                argument_names: &named,
                argument_types: &argument_types,
                explicit_variadic: true,
            },
        )
        .unwrap()
        .is_none());

        let mut generic_variadic = parameter("anyarray");
        generic_variadic.variadic = true;
        let passed_through = match_routine_signature(
            &[generic_variadic],
            RoutineCallDescriptor {
                argument_names: &argument_names,
                argument_types: &argument_types,
                explicit_variadic: true,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            passed_through.variadic_mode,
            RoutineVariadicMode::PassThrough
        );

        let mut candidates = vec![passed_through, fixed];
        assert!(rank_function_matches(&mut candidates, &argument_types));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].declared_identity, ["int4[]"]);
    }

    #[test]
    fn fixed_zero_and_defaulted_variadic_zero_remain_ambiguous() {
        let fixed = match_types(&[], &[]).unwrap().unwrap();
        let mut variadic = parameter("int4[]");
        variadic.has_default = true;
        variadic.variadic = true;
        let defaulted = match_types(&[variadic], &[]).unwrap().unwrap();
        assert_eq!(defaulted.variadic_mode, RoutineVariadicMode::Default);

        let mut candidates = vec![defaulted, fixed];
        assert!(rank_function_matches(&mut candidates, &[]));
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn concrete_and_generic_ranking_matches_postgresql_oracle() {
        let concrete = match_types(&[parameter("int4")], &[Some(ColumnType::Integer)])
            .unwrap()
            .unwrap();
        let generic = match_types(&[parameter("anyelement")], &[Some(ColumnType::Integer)])
            .unwrap()
            .unwrap();
        let mut exact = vec![generic, concrete];
        assert!(rank_function_matches(
            &mut exact,
            &[Some(ColumnType::Integer)]
        ));
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].declared_identity, ["int4"]);

        let concrete = match_types(&[parameter("int4")], &[Some(ColumnType::SmallInteger)])
            .unwrap()
            .unwrap();
        let generic = match_types(
            &[parameter("anyelement")],
            &[Some(ColumnType::SmallInteger)],
        )
        .unwrap()
        .unwrap();
        let mut ambiguous = vec![generic, concrete];
        assert!(rank_function_matches(
            &mut ambiguous,
            &[Some(ColumnType::SmallInteger)]
        ));
        assert_eq!(ambiguous.len(), 2);
    }
}
