//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Polymorphic-family collection, substitution, and coercion targets.

use uqa_sql::ast::{ColumnType, RangeSubtype};

use super::super::common::{base_type, common_type};
use super::super::overload_resolution::{
    canonical_column_type_name, canonical_routine_type_name, routine_type_accepts_implicit_cast,
};
use super::{
    routine_polymorphic_type, RoutineCoercionTarget, RoutinePolymorphicType,
    RoutineTypeSubstitutions,
};

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
        RoutinePolymorphicType::AnyEnum => false,
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

pub(super) fn resolve_target(
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
