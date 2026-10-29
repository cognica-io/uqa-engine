//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::{
    RoutineDefaultType, RoutinePolymorphicFamily, RoutineSignatureMatchError as Error,
};
use super::super::{copy_default, signature_error, Result};
use super::{
    array_actual, array_type, base_type, canonical, common_type_with_control, inline_type,
    is_array_actual, normalize, range_actual, resource_failure, Substitutions, P,
};
use crate::ColumnType;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::type_resolution::routine_signature::production) fn default_substitutions(
    inputs: &[(P, Option<RoutineDefaultType>)],
    control: &ProductionControl<'_>,
) -> Result<Substitutions> {
    let mut simple = SimpleInputs::default();
    let mut compatible = CompatibleInputs {
        elements: ProductionVec::new(*control),
        range: None,
        multirange: None,
    };
    for (declared, actual) in inputs {
        control.check()?;
        let Some(actual) = actual else {
            continue;
        };
        match declared.family() {
            RoutinePolymorphicFamily::Simple => simple.accept(*declared, actual, control)?,
            RoutinePolymorphicFamily::Compatible => {
                compatible.accept(*declared, actual, control)?;
            }
        }
    }
    let mut substitutions = Substitutions::default();
    simple.finish(inputs, &mut substitutions, control)?;
    compatible.finish(inputs, &mut substitutions, control)?;
    Ok(substitutions)
}

#[derive(Default)]
struct SimpleInputs {
    element: Option<Produced<RoutineDefaultType>>,
    array: Option<Produced<RoutineDefaultType>>,
    range: Option<Produced<RoutineDefaultType>>,
    multirange: Option<Produced<RoutineDefaultType>>,
}

impl SimpleInputs {
    fn accept(
        &mut self,
        declared: P,
        actual: &RoutineDefaultType,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        let (slot, name, flatten) = match declared {
            P::AnyElement | P::AnyNonArray | P::AnyEnum => (&mut self.element, "anyelement", false),
            P::AnyArray => (&mut self.array, "anyarray", true),
            P::AnyRange => (&mut self.range, "anyrange", true),
            P::AnyMultirange => (&mut self.multirange, "anymultirange", true),
            _ => unreachable!("simple-family input"),
        };
        retain_same_type(
            slot,
            normalized_actual(actual, flatten, control)?,
            name,
            control,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "simple-family substitution checks preserve the order of array, range and element diagnostics"
    )]
    fn finish(
        mut self,
        inputs: &[(P, Option<RoutineDefaultType>)],
        substitutions: &mut Substitutions,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        let count = inputs
            .iter()
            .filter(|(declared, _)| declared.family() == RoutinePolymorphicFamily::Simple)
            .count();
        if count == 0 {
            return Ok(());
        }
        if let Some(array) = &self.array {
            if matches!(&**array, RoutineDefaultType::Polymorphic(name) if name == "anyarray")
                || matches!(&**array, RoutineDefaultType::Concrete(ColumnType::AnyArray))
            {
                if count != 1 {
                    return Err(signature_error(control, |_| {
                        Ok(Error::IndeterminateArrayElement)
                    }));
                }
                substitutions.simple_array = Some(inline_type(ColumnType::AnyArray, control)?);
                return Ok(());
            }
            let pair = match concrete_actual(array) {
                Some(actual) => array_actual(actual, control)?,
                None => None,
            };
            let Some((element, array_type)) = pair else {
                return Err(invalid_actual("anyarray", array, control));
            };
            Self::merge_element(&mut self.element, element, array, "anyarray", control)?;
            substitutions.simple_array = Some(array_type);
        }
        if let Some(multirange) = &self.multirange {
            let subtype = concrete_actual(multirange)
                .and_then(|actual| range_actual(actual, true))
                .ok_or_else(|| invalid_actual("anymultirange", multirange, control))?;
            let range = inline_default(ColumnType::Range(subtype), control)?;
            if let Some(existing) = &self.range {
                if !same_actual_type(existing, &range, control)? {
                    return Err(inconsistent_types(
                        "anymultirange",
                        "anyrange",
                        multirange,
                        existing,
                        control,
                    ));
                }
            } else {
                self.range = Some(range);
            }
        }
        if let Some(range) = &self.range {
            let subtype = concrete_actual(range)
                .and_then(|actual| range_actual(actual, false))
                .ok_or_else(|| invalid_actual("anyrange", range, control))?;
            Self::merge_element(
                &mut self.element,
                subtype.scalar_type().clone_with_control(control)?,
                range,
                "anyrange",
                control,
            )?;
            substitutions.simple_range = Some(inline_type(ColumnType::Range(subtype), control)?);
            substitutions.simple_multirange =
                Some(inline_type(ColumnType::Multirange(subtype), control)?);
        }
        let Some(element) = self.element.as_deref().and_then(concrete_actual) else {
            return Err(signature_error(control, |_| {
                Ok(Error::IndeterminatePolymorphicType {
                    family: RoutinePolymorphicFamily::Simple,
                })
            }));
        };
        for (declared, _) in inputs {
            control.check()?;
            let invalid = match declared {
                P::AnyNonArray => is_array_actual(element),
                P::AnyEnum => !matches!(element, ColumnType::Enum(_)),
                _ => false,
            };
            if invalid {
                return Err(signature_error(control, |fields| {
                    Ok(Error::InvalidPolymorphicElement {
                        declared: fields.text(polymorphic_name(*declared))?,
                        actual: fields.ty(element)?,
                    })
                }));
            }
            if declared.is_range_family()
                && declared.family() == RoutinePolymorphicFamily::Simple
                && self.range.is_none()
            {
                return Err(signature_error(control, |fields| {
                    Ok(Error::IndeterminatePolymorphicArgument {
                        declared: fields.text(polymorphic_name(*declared))?,
                    })
                }));
            }
        }
        if substitutions.simple_array.is_none() {
            substitutions.simple_array = Some(array_type(element, control)?);
        }
        substitutions.simple_element = Some(element.clone_with_control(control)?);
        Ok(())
    }

    fn merge_element(
        slot: &mut Option<Produced<RoutineDefaultType>>,
        element: Produced<ColumnType>,
        container: &RoutineDefaultType,
        declared: &str,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        let (element, memory) = element.into_parts();
        let element = control.finish(RoutineDefaultType::Concrete(element), memory)?;
        if let Some(existing) = slot.as_ref() {
            if !same_actual_type(existing, &element, control)? {
                return Err(inconsistent_types(
                    declared,
                    "anyelement",
                    container,
                    existing,
                    control,
                ));
            }
        } else {
            *slot = Some(element);
        }
        Ok(())
    }
}

struct CompatibleInputs<'a> {
    elements: ProductionVec<'a, Produced<ColumnType>>,
    range: Option<Produced<RoutineDefaultType>>,
    multirange: Option<Produced<RoutineDefaultType>>,
}

impl CompatibleInputs<'_> {
    fn push(&mut self, value: Produced<ColumnType>, control: &ProductionControl<'_>) -> Result<()> {
        self.elements
            .push_produced(control.finish(value, control.empty_reservation())?)?;
        Ok(())
    }

    fn accept(
        &mut self,
        declared: P,
        actual: &RoutineDefaultType,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        match declared {
            P::AnyCompatible | P::AnyCompatibleNonArray => {
                if let Some(actual) = concrete_actual(actual) {
                    self.push(normalize(actual, control)?, control)?;
                }
            }
            P::AnyCompatibleArray => {
                let pair = match concrete_actual(actual) {
                    Some(actual) => array_actual(actual, control)?,
                    None => None,
                };
                let (element, _) =
                    pair.ok_or_else(|| invalid_actual("anycompatiblearray", actual, control))?;
                self.push(element, control)?;
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
                    normalized_actual(actual, true, control)?,
                    polymorphic_name(declared),
                    control,
                )?;
                let subtype = concrete_actual(actual)
                    .and_then(|actual| range_actual(actual, multi))
                    .ok_or_else(|| invalid_actual(polymorphic_name(declared), actual, control))?;
                if !multi && first {
                    self.push(subtype.scalar_type().clone_with_control(control)?, control)?;
                }
            }
            _ => unreachable!("compatible-family input"),
        }
        Ok(())
    }

    fn finish(
        mut self,
        inputs: &[(P, Option<RoutineDefaultType>)],
        substitutions: &mut Substitutions,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        if !inputs
            .iter()
            .any(|(declared, _)| declared.family() == RoutinePolymorphicFamily::Compatible)
        {
            return Ok(());
        }
        if let Some(multirange) = &self.multirange {
            let subtype = concrete_actual(multirange)
                .and_then(|actual| range_actual(actual, true))
                .ok_or_else(|| invalid_actual("anycompatiblemultirange", multirange, control))?;
            let range = inline_default(ColumnType::Range(subtype), control)?;
            if let Some(existing) = &self.range {
                if !same_actual_type(existing, &range, control)? {
                    return Err(inconsistent_types(
                        "anycompatiblemultirange",
                        "anycompatiblerange",
                        multirange,
                        existing,
                        control,
                    ));
                }
            } else {
                self.range = Some(range);
                self.push(subtype.scalar_type().clone_with_control(control)?, control)?;
            }
        }
        let mut elements = super::super::owned_iterator(self.elements.finish()?);
        let mut common = match elements.next() {
            Some(value) => value,
            None => inline_type(ColumnType::Text, control)?,
        };
        for element in elements {
            match common_type_with_control(&common, &element, control) {
                Ok(selected) => common = selected,
                Err(error) if resource_failure(&error) => return Err(error.into()),
                Err(_) => {
                    return Err(signature_error(control, |fields| {
                        Ok(Error::IncompatibleDefaultTypes {
                            first: fields.boxed_type(&common)?,
                            second: fields.boxed_type(&element)?,
                        })
                    }))
                }
            }
        }
        for (declared, _) in inputs {
            control.check()?;
            if matches!(declared, P::AnyCompatibleRange | P::AnyCompatibleMultirange) {
                let Some(range) = &self.range else {
                    return Err(signature_error(control, |fields| {
                        Ok(Error::IndeterminatePolymorphicArgument {
                            declared: fields.text(polymorphic_name(*declared))?,
                        })
                    }));
                };
                let range_type = concrete_actual(range).expect("range inputs validated");
                let subtype = range_actual(range_type, false).expect("range inputs validated");
                if *canonical(&subtype.scalar_type(), control)? != *canonical(&common, control)? {
                    let range_type = if *declared == P::AnyCompatibleMultirange {
                        Some(ColumnType::Multirange(subtype))
                    } else {
                        None
                    };
                    return Err(signature_error(control, |fields| {
                        Ok(Error::IncompatibleRangeSubtype {
                            declared: fields.text(polymorphic_name(*declared))?,
                            range: fields.boxed_type(range_type.as_ref().unwrap_or_else(|| {
                                concrete_actual(range).expect("concrete range")
                            }))?,
                            common: fields.boxed_type(&common)?,
                        })
                    }));
                }
            }
            if *declared == P::AnyCompatibleNonArray && is_array_actual(&common) {
                return Err(signature_error(control, |fields| {
                    Ok(Error::InvalidPolymorphicElement {
                        declared: fields.text("anycompatiblenonarray")?,
                        actual: fields.ty(&common)?,
                    })
                }));
            }
        }
        if let Some(range) = self.range.as_deref().and_then(concrete_actual) {
            let subtype = range_actual(range, false).expect("range inputs validated");
            substitutions.compatible_range =
                Some(inline_type(ColumnType::Range(subtype), control)?);
            substitutions.compatible_multirange =
                Some(inline_type(ColumnType::Multirange(subtype), control)?);
        }
        substitutions.compatible_array = Some(array_type(&common, control)?);
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

fn normalized_actual(
    actual: &RoutineDefaultType,
    flatten: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoutineDefaultType>> {
    match actual {
        RoutineDefaultType::Concrete(ty) => {
            let (ty, memory) =
                normalize(if flatten { base_type(ty) } else { ty }, control)?.into_parts();
            Ok(control.finish(RoutineDefaultType::Concrete(ty), memory)?)
        }
        RoutineDefaultType::Polymorphic(_) => Ok(copy_default(actual, control)?),
    }
}

fn inline_default(
    ty: ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoutineDefaultType>> {
    let (ty, memory) = inline_type(ty, control)?.into_parts();
    Ok(control.finish(RoutineDefaultType::Concrete(ty), memory)?)
}

fn same_actual_type(
    first: &RoutineDefaultType,
    second: &RoutineDefaultType,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(match (first, second) {
        (RoutineDefaultType::Concrete(first), RoutineDefaultType::Concrete(second)) => {
            *canonical(first, control)? == *canonical(second, control)?
        }
        _ => first == second,
    })
}

fn retain_same_type(
    slot: &mut Option<Produced<RoutineDefaultType>>,
    actual: Produced<RoutineDefaultType>,
    declared: &str,
    control: &ProductionControl<'_>,
) -> Result<()> {
    if let Some(existing) = slot {
        if !same_actual_type(existing, &actual, control)? {
            return Err(signature_error(control, |fields| {
                Ok(Error::InconsistentDefault {
                    declared: fields.text(declared)?,
                    first: fields.boxed_default(existing)?,
                    second: fields.boxed_default(&actual)?,
                })
            }));
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
    control: &ProductionControl<'_>,
) -> super::super::RoutineSignatureProductionError {
    signature_error(control, |fields| {
        Ok(Error::InconsistentPolymorphicTypes {
            first_declared: fields.text(first_declared)?,
            second_declared: fields.text(second_declared)?,
            first: fields.boxed_default(first)?,
            second: fields.boxed_default(second)?,
        })
    })
}

fn invalid_actual(
    declared: &str,
    actual: &RoutineDefaultType,
    control: &ProductionControl<'_>,
) -> super::super::RoutineSignatureProductionError {
    signature_error(control, |fields| {
        Ok(Error::InvalidPolymorphicActual {
            declared: fields.text(declared)?,
            actual: fields.default_type(actual)?,
        })
    })
}

fn polymorphic_name(ty: P) -> &'static str {
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
