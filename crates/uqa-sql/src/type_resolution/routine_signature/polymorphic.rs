//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Polymorphic-family collection, substitution, and coercion targets.

use crate::ast::{ColumnType, RangeSubtype, RoutineDefaultType};

use super::super::common::{base_type, common_type};
use super::super::overload_resolution::{
    canonical_column_type_name, canonical_routine_type_name, routine_type_accepts_implicit_cast,
};
use super::{
    routine_polymorphic_type, RoutineCoercionTarget, RoutinePolymorphicType,
    RoutineSignatureMatchError, RoutineTypeSubstitutions,
};

/// Enforce the selected call's polymorphic constraints after its omitted defaults
/// have been appended. Candidate ranking sees only the supplied arguments.
pub(super) fn default_substitutions(
    inputs: &[(RoutinePolymorphicType, Option<RoutineDefaultType>)],
) -> Result<RoutineTypeSubstitutions, RoutineSignatureMatchError> {
    let mut simple = SimpleInputs::default();
    let mut compatible = CompatibleInputs::default();
    for (declared, actual) in inputs {
        let Some(actual) = actual else { continue };
        match declared.family() {
            super::RoutinePolymorphicFamily::Simple => simple.accept(*declared, actual)?,
            super::RoutinePolymorphicFamily::Compatible => compatible.accept(*declared, actual)?,
        }
    }
    let mut substitutions = RoutineTypeSubstitutions::default();
    simple.finish(inputs, &mut substitutions)?;
    compatible.finish(inputs, &mut substitutions)?;
    Ok(substitutions)
}

#[derive(Default)]
struct SimpleInputs {
    element: Option<RoutineDefaultType>,
    array: Option<RoutineDefaultType>,
    range: Option<RoutineDefaultType>,
    multirange: Option<RoutineDefaultType>,
}

impl SimpleInputs {
    fn accept(
        &mut self,
        declared: RoutinePolymorphicType,
        actual: &RoutineDefaultType,
    ) -> Result<(), RoutineSignatureMatchError> {
        let (slot, name, flatten) = match declared {
            RoutinePolymorphicType::AnyElement
            | RoutinePolymorphicType::AnyNonArray
            | RoutinePolymorphicType::AnyEnum => (&mut self.element, "anyelement", false),
            RoutinePolymorphicType::AnyArray => (&mut self.array, "anyarray", true),
            RoutinePolymorphicType::AnyRange => (&mut self.range, "anyrange", true),
            RoutinePolymorphicType::AnyMultirange => (&mut self.multirange, "anymultirange", true),
            _ => unreachable!("simple-family input"),
        };
        retain_same_type(slot, normalized_actual(actual, flatten), name)
    }

    fn finish(
        mut self,
        inputs: &[(RoutinePolymorphicType, Option<RoutineDefaultType>)],
        substitutions: &mut RoutineTypeSubstitutions,
    ) -> Result<(), RoutineSignatureMatchError> {
        use RoutinePolymorphicType as P;
        let simple_count = inputs
            .iter()
            .filter(|(declared, _)| declared.family() == super::RoutinePolymorphicFamily::Simple)
            .count();
        if simple_count == 0 {
            return Ok(());
        }
        if let Some(array) = &self.array {
            if matches!(array, RoutineDefaultType::Polymorphic(name) if name == "anyarray")
                || matches!(array, RoutineDefaultType::Concrete(ColumnType::AnyArray))
            {
                if simple_count != 1 {
                    return Err(RoutineSignatureMatchError::IndeterminateArrayElement);
                }
                substitutions.simple_array = Some(ColumnType::AnyArray);
                return Ok(());
            }
            let Some((element, array_type)) = concrete_actual(array).and_then(array_actual) else {
                return Err(invalid_actual("anyarray", array));
            };
            Self::merge_element(&mut self.element, element, array, "anyarray")?;
            substitutions.simple_array = Some(array_type);
        }
        if let Some(multirange) = &self.multirange {
            let subtype = concrete_actual(multirange)
                .and_then(|actual| range_actual(actual, true))
                .ok_or_else(|| invalid_actual("anymultirange", multirange))?;
            let range = RoutineDefaultType::Concrete(ColumnType::Range(subtype));
            if let Some(existing) = &self.range {
                if !same_actual_type(existing, &range) {
                    return Err(inconsistent_types(
                        "anymultirange",
                        "anyrange",
                        multirange,
                        existing,
                    ));
                }
            } else {
                self.range = Some(range);
            }
        }
        if let Some(range) = &self.range {
            let subtype = concrete_actual(range)
                .and_then(|actual| range_actual(actual, false))
                .ok_or_else(|| invalid_actual("anyrange", range))?;
            Self::merge_element(&mut self.element, subtype.scalar_type(), range, "anyrange")?;
            substitutions.simple_range = Some(ColumnType::Range(subtype));
            substitutions.simple_multirange = Some(ColumnType::Multirange(subtype));
        }
        let Some(element) = self.element.as_ref().and_then(concrete_actual) else {
            return Err(RoutineSignatureMatchError::IndeterminatePolymorphicType {
                family: super::RoutinePolymorphicFamily::Simple,
            });
        };
        for (declared, _) in inputs {
            let invalid = match declared {
                P::AnyNonArray => is_array_actual(element),
                P::AnyEnum => !matches!(element, ColumnType::Enum(_)),
                _ => false,
            };
            if invalid {
                return Err(RoutineSignatureMatchError::InvalidPolymorphicElement {
                    declared: polymorphic_name(*declared).into(),
                    actual: element.clone(),
                });
            }
            if declared.is_range_family()
                && declared.family() == super::RoutinePolymorphicFamily::Simple
                && self.range.is_none()
            {
                return Err(
                    RoutineSignatureMatchError::IndeterminatePolymorphicArgument {
                        declared: polymorphic_name(*declared).into(),
                    },
                );
            }
        }
        substitutions
            .simple_array
            .get_or_insert_with(|| ColumnType::Array(Box::new(element.clone())));
        substitutions.simple_element = Some(element.clone());
        Ok(())
    }

    fn merge_element(
        slot: &mut Option<RoutineDefaultType>,
        element: ColumnType,
        container: &RoutineDefaultType,
        declared: &str,
    ) -> Result<(), RoutineSignatureMatchError> {
        let element = RoutineDefaultType::Concrete(element);
        if let Some(existing) = slot.as_ref() {
            if !same_actual_type(existing, &element) {
                return Err(inconsistent_types(
                    declared,
                    "anyelement",
                    container,
                    existing,
                ));
            }
        } else {
            *slot = Some(element);
        }
        Ok(())
    }
}

#[derive(Default)]
struct CompatibleInputs {
    elements: Vec<ColumnType>,
    range: Option<RoutineDefaultType>,
    multirange: Option<RoutineDefaultType>,
}

impl CompatibleInputs {
    fn accept(
        &mut self,
        declared: RoutinePolymorphicType,
        actual: &RoutineDefaultType,
    ) -> Result<(), RoutineSignatureMatchError> {
        use RoutinePolymorphicType as P;
        match declared {
            P::AnyCompatible | P::AnyCompatibleNonArray => {
                if let Some(actual) = concrete_actual(actual) {
                    self.elements.push(normalize_identity_type(actual));
                }
            }
            P::AnyCompatibleArray => {
                let element = concrete_actual(actual)
                    .and_then(array_actual)
                    .map(|(element, _)| element)
                    .ok_or_else(|| invalid_actual("anycompatiblearray", actual))?;
                self.elements.push(element);
            }
            P::AnyCompatibleRange | P::AnyCompatibleMultirange => {
                let multi = declared == P::AnyCompatibleMultirange;
                let slot = if multi {
                    &mut self.multirange
                } else {
                    &mut self.range
                };
                let first = slot.is_none();
                retain_same_type(
                    slot,
                    normalized_actual(actual, true),
                    polymorphic_name(declared),
                )?;
                let subtype = concrete_actual(actual)
                    .and_then(|actual| range_actual(actual, multi))
                    .ok_or_else(|| invalid_actual(polymorphic_name(declared), actual))?;
                if !multi && first {
                    self.elements.push(subtype.scalar_type());
                }
            }
            _ => unreachable!("compatible-family input"),
        }
        Ok(())
    }

    fn finish(
        mut self,
        inputs: &[(RoutinePolymorphicType, Option<RoutineDefaultType>)],
        substitutions: &mut RoutineTypeSubstitutions,
    ) -> Result<(), RoutineSignatureMatchError> {
        use RoutinePolymorphicType as P;
        if !inputs
            .iter()
            .any(|(declared, _)| declared.family() == super::RoutinePolymorphicFamily::Compatible)
        {
            return Ok(());
        }
        if let Some(multirange) = &self.multirange {
            let subtype = concrete_actual(multirange)
                .and_then(|actual| range_actual(actual, true))
                .ok_or_else(|| invalid_actual("anycompatiblemultirange", multirange))?;
            let range = RoutineDefaultType::Concrete(ColumnType::Range(subtype));
            if let Some(existing) = &self.range {
                if !same_actual_type(existing, &range) {
                    return Err(inconsistent_types(
                        "anycompatiblemultirange",
                        "anycompatiblerange",
                        multirange,
                        existing,
                    ));
                }
            } else {
                self.range = Some(range);
                self.elements.push(subtype.scalar_type());
            }
        }
        let mut elements = self.elements.into_iter();
        let mut common = elements.next().unwrap_or(ColumnType::Text);
        for element in elements {
            common = common_type(&common, &element).map_err(|_| {
                RoutineSignatureMatchError::IncompatibleDefaultTypes {
                    first: Box::new(common.clone()),
                    second: Box::new(element),
                }
            })?;
        }
        for (declared, _) in inputs {
            if matches!(declared, P::AnyCompatibleRange | P::AnyCompatibleMultirange) {
                let Some(range) = &self.range else {
                    return Err(
                        RoutineSignatureMatchError::IndeterminatePolymorphicArgument {
                            declared: polymorphic_name(*declared).into(),
                        },
                    );
                };
                let range_type = concrete_actual(range).expect("range inputs were validated");
                let subtype = range_actual(range_type, false).expect("range inputs were validated");
                if canonical_column_type_name(&subtype.scalar_type())
                    != canonical_column_type_name(&common)
                {
                    return Err(RoutineSignatureMatchError::IncompatibleRangeSubtype {
                        declared: polymorphic_name(*declared).into(),
                        range: Box::new(if *declared == P::AnyCompatibleMultirange {
                            ColumnType::Multirange(subtype)
                        } else {
                            range_type.clone()
                        }),
                        common: Box::new(common),
                    });
                }
            }
            if *declared == P::AnyCompatibleNonArray && is_array_actual(&common) {
                return Err(RoutineSignatureMatchError::InvalidPolymorphicElement {
                    declared: "anycompatiblenonarray".into(),
                    actual: common,
                });
            }
        }
        if let Some(range) = self.range.as_ref().and_then(concrete_actual) {
            let subtype = range_actual(range, false).expect("range inputs were validated");
            substitutions.compatible_range = Some(ColumnType::Range(subtype));
            substitutions.compatible_multirange = Some(ColumnType::Multirange(subtype));
        }
        substitutions.compatible_array = Some(ColumnType::Array(Box::new(common.clone())));
        substitutions.compatible_element = Some(common);
        Ok(())
    }
}

fn concrete_actual(actual: &RoutineDefaultType) -> Option<&ColumnType> {
    match actual {
        RoutineDefaultType::Concrete(ty) => Some(ty),
        RoutineDefaultType::Polymorphic(_) => None,
    }
}

fn normalized_actual(actual: &RoutineDefaultType, flatten: bool) -> RoutineDefaultType {
    match actual {
        RoutineDefaultType::Concrete(ty) => {
            RoutineDefaultType::Concrete(normalize_identity_type(if flatten {
                base_type(ty)
            } else {
                ty
            }))
        }
        RoutineDefaultType::Polymorphic(_) => actual.clone(),
    }
}

fn same_actual_type(first: &RoutineDefaultType, second: &RoutineDefaultType) -> bool {
    match (first, second) {
        (RoutineDefaultType::Concrete(first), RoutineDefaultType::Concrete(second)) => {
            canonical_column_type_name(first) == canonical_column_type_name(second)
        }
        _ => first == second,
    }
}

fn retain_same_type(
    slot: &mut Option<RoutineDefaultType>,
    actual: RoutineDefaultType,
    declared: &str,
) -> Result<(), RoutineSignatureMatchError> {
    if let Some(existing) = slot {
        if !same_actual_type(existing, &actual) {
            return Err(RoutineSignatureMatchError::InconsistentDefault {
                declared: declared.into(),
                first: Box::new(existing.clone()),
                second: Box::new(actual),
            });
        }
    } else {
        *slot = Some(actual);
    }
    Ok(())
}

fn inconsistent_types(
    first_declared: &str,
    second_declared: &str,
    first: &RoutineDefaultType,
    second: &RoutineDefaultType,
) -> RoutineSignatureMatchError {
    RoutineSignatureMatchError::InconsistentPolymorphicTypes {
        first_declared: first_declared.into(),
        second_declared: second_declared.into(),
        first: Box::new(first.clone()),
        second: Box::new(second.clone()),
    }
}

fn invalid_actual(declared: &str, actual: &RoutineDefaultType) -> RoutineSignatureMatchError {
    RoutineSignatureMatchError::InvalidPolymorphicActual {
        declared: declared.into(),
        actual: actual.clone(),
    }
}

fn polymorphic_name(ty: RoutinePolymorphicType) -> &'static str {
    use RoutinePolymorphicType as P;
    match ty {
        P::AnyElement => "anyelement",
        P::AnyArray => "anyarray",
        P::AnyNonArray => "anynonarray",
        P::AnyEnum => "anyenum",
        P::AnyRange => "anyrange",
        P::AnyMultirange => "anymultirange",
        P::AnyCompatible => "anycompatible",
        P::AnyCompatibleArray => "anycompatiblearray",
        P::AnyCompatibleNonArray => "anycompatiblenonarray",
        P::AnyCompatibleRange => "anycompatiblerange",
        P::AnyCompatibleMultirange => "anycompatiblemultirange",
    }
}

pub(super) fn collect_polymorphic_actual(
    polymorphic: RoutinePolymorphicType,
    actual: &ColumnType,
    simple_element: &mut Option<ColumnType>,
    simple_array: &mut Option<ColumnType>,
    simple_range_subtype: &mut Option<RangeSubtype>,
    compatible_element: &mut Option<ColumnType>,
    compatible_range_seen: &mut bool,
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
        RoutinePolymorphicType::AnyRange => {
            let Some(subtype) = range_actual(actual, false) else {
                return false;
            };
            merge_range_subtype(simple_range_subtype, subtype)
                && merge_same_identity(simple_element, subtype.scalar_type())
        }
        RoutinePolymorphicType::AnyMultirange => {
            let Some(subtype) = range_actual(actual, true) else {
                return false;
            };
            merge_range_subtype(simple_range_subtype, subtype)
                && merge_same_identity(simple_element, subtype.scalar_type())
        }
        RoutinePolymorphicType::AnyCompatibleRange => {
            let Some(subtype) = range_actual(actual, false) else {
                return false;
            };
            *compatible_range_seen = true;
            merge_compatible(compatible_element, subtype.scalar_type())
        }
        RoutinePolymorphicType::AnyCompatibleMultirange => {
            let Some(subtype) = range_actual(actual, true) else {
                return false;
            };
            *compatible_range_seen = true;
            merge_compatible(compatible_element, subtype.scalar_type())
        }
        // `anyenum` shares the simple family's element type and requires an enum; a domain over an enum is not an enum type.
        RoutinePolymorphicType::AnyEnum => {
            matches!(actual, ColumnType::Enum(_))
                && merge_same_identity(simple_element, actual.clone())
        }
    }
}

fn merge_range_subtype(slot: &mut Option<RangeSubtype>, candidate: RangeSubtype) -> bool {
    if let Some(current) = slot {
        *current == candidate
    } else {
        *slot = Some(candidate);
        true
    }
}

fn range_actual(actual: &ColumnType, multirange: bool) -> Option<RangeSubtype> {
    match (base_type(actual), multirange) {
        (ColumnType::Range(subtype), false) | (ColumnType::Multirange(subtype), true) => {
            Some(*subtype)
        }
        _ => None,
    }
}

pub(super) fn range_subtype_for_scalar(actual: &ColumnType) -> Option<RangeSubtype> {
    match base_type(actual) {
        ColumnType::Integer => Some(RangeSubtype::Integer),
        ColumnType::BigInteger => Some(RangeSubtype::BigInteger),
        ColumnType::Numeric { .. } => Some(RangeSubtype::Numeric),
        ColumnType::Date => Some(RangeSubtype::Date),
        ColumnType::Timestamp => Some(RangeSubtype::Timestamp),
        ColumnType::TimestampTz => Some(RangeSubtype::TimestampTz),
        _ => None,
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
        Some(current) => {
            if let Ok(common) = common_type(&current, &candidate) {
                *slot = Some(normalize_identity_type(&common));
            } else {
                *slot = Some(current);
                return false;
            }
        }
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

pub(super) fn resolve_target(
    declared_type_name: &str,
    declared: Option<&ColumnType>,
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
    // A catalog enum declaration is identified by OID, independent of its spelling and of the search path at the call.
    if let Some(declared) =
        declared.filter(|declared| crate::expr::enums::is_enum_bearing(declared))
    {
        return Some(RoutineCoercionTarget {
            type_name: canonical_column_type_name(declared),
            column_type: Some(declared.clone()),
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

pub(super) fn actual_accepts_polymorphic_target(
    actual: &ColumnType,
    target: &RoutineCoercionTarget,
) -> bool {
    let raw_actual = canonical_column_type_name(actual);
    let base_actual = canonical_column_type_name(base_type(actual));
    raw_actual == target.type_name
        || base_actual == target.type_name
        || routine_type_accepts_implicit_cast(&base_actual, &target.type_name)
}
