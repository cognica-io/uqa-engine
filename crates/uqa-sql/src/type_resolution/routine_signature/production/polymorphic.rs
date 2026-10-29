//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each inferred polymorphic type keeps its own lease while candidate construction replaces or discards alternatives.

use super::super::super::{
    common::{base_type, common_type_with_control},
    overload_resolution::{
        canonical_column_type_name_with_control as canonical,
        canonical_routine_type_name_with_control, routine_type_accepts_implicit_cast,
    },
};
use super::super::{RoutineCoercionTarget, RoutinePolymorphicType as P, RoutineTypeSubstitutions};
use super::{
    array_type, inline_type, parsed_type, polymorphic_type, resource_failure, Components, Result,
};
use crate::ast::{ColumnType, RangeSubtype};
use uqa_core::memory::{Produced, ProductionControl};

mod defaults;
pub(super) use defaults::default_substitutions;

#[derive(Default)]
pub(super) struct Substitutions {
    pub(super) simple_element: Option<Produced<ColumnType>>,
    pub(super) simple_array: Option<Produced<ColumnType>>,
    pub(super) simple_range: Option<Produced<ColumnType>>,
    pub(super) simple_multirange: Option<Produced<ColumnType>>,
    pub(super) compatible_element: Option<Produced<ColumnType>>,
    pub(super) compatible_array: Option<Produced<ColumnType>>,
    pub(super) compatible_range: Option<Produced<ColumnType>>,
    pub(super) compatible_multirange: Option<Produced<ColumnType>>,
}

impl Substitutions {
    fn get(&self, ty: P) -> Option<&ColumnType> {
        match ty {
            P::AnyElement | P::AnyNonArray | P::AnyEnum => self.simple_element.as_deref(),
            P::AnyArray => self.simple_array.as_deref(),
            P::AnyRange => self.simple_range.as_deref(),
            P::AnyMultirange => self.simple_multirange.as_deref(),
            P::AnyCompatible | P::AnyCompatibleNonArray => self.compatible_element.as_deref(),
            P::AnyCompatibleArray => self.compatible_array.as_deref(),
            P::AnyCompatibleRange => self.compatible_range.as_deref(),
            P::AnyCompatibleMultirange => self.compatible_multirange.as_deref(),
        }
    }
    pub(super) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoutineTypeSubstitutions>> {
        let mut fields = Components::new(*control);
        let value = RoutineTypeSubstitutions {
            simple_element: self.simple_element.map(|value| fields.take(value)),
            simple_array: self.simple_array.map(|value| fields.take(value)),
            simple_range: self.simple_range.map(|value| fields.take(value)),
            simple_multirange: self.simple_multirange.map(|value| fields.take(value)),
            compatible_element: self.compatible_element.map(|value| fields.take(value)),
            compatible_array: self.compatible_array.map(|value| fields.take(value)),
            compatible_range: self.compatible_range.map(|value| fields.take(value)),
            compatible_multirange: self.compatible_multirange.map(|value| fields.take(value)),
        };
        Ok(fields.finish(value)?)
    }
}

pub(super) struct Target {
    pub(super) name: Produced<String>,
    pub(super) ty: Option<Produced<ColumnType>>,
}

impl Target {
    pub(super) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoutineCoercionTarget>> {
        let mut fields = Components::new(*control);
        let value = RoutineCoercionTarget {
            type_name: fields.take(self.name),
            column_type: self.ty.map(|ty| fields.take(ty)),
        };
        Ok(fields.finish(value)?)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "independent simple and compatible family slots retain their own admitted type owners"
)]
pub(super) fn collect_polymorphic_actual(
    polymorphic: P,
    actual: &ColumnType,
    simple_element: &mut Option<Produced<ColumnType>>,
    simple_array: &mut Option<Produced<ColumnType>>,
    simple_range_subtype: &mut Option<RangeSubtype>,
    compatible_element: &mut Option<Produced<ColumnType>>,
    compatible_range_seen: &mut bool,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    control.check()?;
    match polymorphic {
        P::AnyElement => merge_same_identity(simple_element, normalize(actual, control)?, control),
        P::AnyNonArray => {
            if is_array_actual(actual) {
                return Ok(false);
            }
            merge_same_identity(simple_element, normalize(actual, control)?, control)
        }
        P::AnyArray => {
            let Some((element, array)) = array_actual(actual, control)? else {
                return Ok(false);
            };
            Ok(merge_same_identity(simple_element, element, control)?
                && merge_same_identity(simple_array, array, control)?)
        }
        P::AnyCompatible => {
            merge_compatible(compatible_element, normalize(actual, control)?, control)
        }
        P::AnyCompatibleNonArray => {
            if is_array_actual(actual) {
                return Ok(false);
            }
            merge_compatible(compatible_element, normalize(actual, control)?, control)
        }
        P::AnyCompatibleArray => {
            let Some((element, _)) = array_actual(actual, control)? else {
                return Ok(false);
            };
            merge_compatible(compatible_element, element, control)
        }
        P::AnyRange | P::AnyMultirange => {
            let Some(subtype) = range_actual(actual, polymorphic == P::AnyMultirange) else {
                return Ok(false);
            };
            if simple_range_subtype.is_some_and(|current| current != subtype) {
                return Ok(false);
            }
            *simple_range_subtype = Some(subtype);
            merge_same_identity(
                simple_element,
                subtype.scalar_type().clone_with_control(control)?,
                control,
            )
        }
        P::AnyCompatibleRange | P::AnyCompatibleMultirange => {
            let Some(subtype) = range_actual(actual, polymorphic == P::AnyCompatibleMultirange)
            else {
                return Ok(false);
            };
            *compatible_range_seen = true;
            merge_compatible(
                compatible_element,
                subtype.scalar_type().clone_with_control(control)?,
                control,
            )
        }
        P::AnyEnum => {
            if !matches!(actual, ColumnType::Enum(_)) {
                return Ok(false);
            }
            merge_same_identity(simple_element, actual.clone_with_control(control)?, control)
        }
    }
}

fn merge_same_identity(
    slot: &mut Option<Produced<ColumnType>>,
    candidate: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    if let Some(current) = slot {
        Ok(*canonical(current, control)? == *canonical(&candidate, control)?)
    } else {
        *slot = Some(candidate);
        Ok(true)
    }
}

fn merge_compatible(
    slot: &mut Option<Produced<ColumnType>>,
    candidate: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    match slot.as_ref() {
        None => *slot = Some(candidate),
        Some(current) => match common_type_with_control(current, &candidate, control) {
            Ok(common) => *slot = Some(normalize(&common, control)?),
            Err(error) if resource_failure(&error) => return Err(error.into()),
            Err(_) => return Ok(false),
        },
    }
    Ok(true)
}

fn normalize(actual: &ColumnType, control: &ProductionControl<'_>) -> Result<Produced<ColumnType>> {
    if matches!(actual, ColumnType::Domain { .. }) {
        return Ok(actual.clone_with_control(control)?);
    }
    let name = canonical(actual, control)?;
    Ok(match parsed_type(&name, control)? {
        Some(ty) => ty,
        None => actual.clone_with_control(control)?,
    })
}

fn array_actual(
    actual: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<(Produced<ColumnType>, Produced<ColumnType>)>> {
    let actual = base_type(actual);
    Ok(match actual {
        ColumnType::Array(element) => {
            Some((normalize(element, control)?, normalize(actual, control)?))
        }
        ColumnType::Int2Vector => Some((
            inline_type(ColumnType::SmallInteger, control)?,
            inline_type(ColumnType::Int2Vector, control)?,
        )),
        ColumnType::OidVector => Some((
            inline_type(ColumnType::Oid, control)?,
            inline_type(ColumnType::OidVector, control)?,
        )),
        _ => None,
    })
}

fn is_array_actual(actual: &ColumnType) -> bool {
    matches!(
        base_type(actual),
        ColumnType::Array(_)
            | ColumnType::Int2Vector
            | ColumnType::OidVector
            | ColumnType::AnyArray
    )
}

fn range_actual(actual: &ColumnType, multirange: bool) -> Option<RangeSubtype> {
    match (base_type(actual), multirange) {
        (ColumnType::Range(subtype), false) | (ColumnType::Multirange(subtype), true) => {
            Some(*subtype)
        }
        _ => None,
    }
}

pub(super) fn resolve_target(
    declared_name: &str,
    declared: Option<&ColumnType>,
    actual: Option<&ColumnType>,
    substitutions: &Substitutions,
    control: &ProductionControl<'_>,
) -> Result<Option<Target>> {
    if let Some(polymorphic) = polymorphic_type(declared_name, control)? {
        let Some(ty) = substitutions.get(polymorphic) else {
            return Ok(None);
        };
        return Ok(Some(Target {
            name: canonical(ty, control)?,
            ty: Some(ty.clone_with_control(control)?),
        }));
    }
    if let Some(declared) =
        declared.filter(|declared| crate::expr::enums::is_enum_bearing(declared))
    {
        return Ok(Some(Target {
            name: canonical(declared, control)?,
            ty: Some(declared.clone_with_control(control)?),
        }));
    }
    let name = canonical_routine_type_name_with_control(declared_name, control)?;
    if *name == "any" {
        return Ok(Some(Target {
            name: match actual {
                Some(actual) => canonical(actual, control)?,
                None => control.copy_text("unknown")?,
            },
            ty: actual
                .map(|actual| actual.clone_with_control(control))
                .transpose()?,
        }));
    }
    let mut selected = None;
    if let Some(actual) = actual {
        if *canonical(actual, control)? == *name {
            selected = Some(actual.clone_with_control(control)?);
        } else if *canonical(base_type(actual), control)? == *name {
            selected = Some(base_type(actual).clone_with_control(control)?);
        }
    }
    if selected.is_none() {
        selected = parsed_type(&name, control)?;
    }
    Ok(Some(Target { name, ty: selected }))
}

pub(super) fn actual_accepts_polymorphic_target(
    actual: &ColumnType,
    target: &Target,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    let raw = canonical(actual, control)?;
    let base = canonical(base_type(actual), control)?;
    Ok(*raw == *target.name
        || *base == *target.name
        || routine_type_accepts_implicit_cast(&base, &target.name))
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
