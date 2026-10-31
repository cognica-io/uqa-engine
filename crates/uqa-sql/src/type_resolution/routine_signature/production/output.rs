//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::overload_resolution::{
    canonical_column_type_name_with_control, canonical_routine_type_name_with_control,
};
use super::super::{
    InvocationVariadicMode, MatchedRoutineSignature, RoutineInvocationBinding,
    RoutinePolymorphicType, RoutineTypeSubstitutions, RoutineVariadicMode, RoutineVariadicPlan,
};
use super::{resource_failure, Components};
use crate::{ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl RoutineTypeSubstitutions {
    pub fn substitute_with_control(
        &self,
        polymorphic: RoutinePolymorphicType,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        control.check()?;
        let value = match polymorphic {
            RoutinePolymorphicType::AnyElement
            | RoutinePolymorphicType::AnyNonArray
            | RoutinePolymorphicType::AnyEnum => &self.simple_element,
            RoutinePolymorphicType::AnyArray => &self.simple_array,
            RoutinePolymorphicType::AnyRange => &self.simple_range,
            RoutinePolymorphicType::AnyMultirange => &self.simple_multirange,
            RoutinePolymorphicType::AnyCompatible
            | RoutinePolymorphicType::AnyCompatibleNonArray => &self.compatible_element,
            RoutinePolymorphicType::AnyCompatibleArray => &self.compatible_array,
            RoutinePolymorphicType::AnyCompatibleRange => &self.compatible_range,
            RoutinePolymorphicType::AnyCompatibleMultirange => &self.compatible_multirange,
        };
        Ok(value
            .as_ref()
            .map(|value| value.clone_with_control(control))
            .transpose()?)
    }
}

impl MatchedRoutineSignature {
    pub fn substitute_type_with_control(
        &self,
        declared_type_name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let canonical = canonical_routine_type_name_with_control(declared_type_name, control)?;
        if let Some(polymorphic) = super::super::polymorphic_type_from_canonical(&canonical) {
            return self
                .substitutions
                .substitute_with_control(polymorphic, control);
        }
        if let Some(ty) = parse_type(&canonical, control)? {
            return Ok(Some(ty));
        }
        Ok(self
            .declared_identity
            .iter()
            .zip(&self.parameter_type_values)
            .find_map(|(declared, resolved)| {
                (declared == &*canonical)
                    .then_some(resolved.as_ref())
                    .flatten()
            })
            .map(|ty| ty.clone_with_control(control))
            .transpose()?)
    }

    pub fn substitute_type_name_with_control(
        &self,
        declared_type_name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        let canonical = canonical_routine_type_name_with_control(declared_type_name, control)?;
        if super::super::polymorphic_type_from_canonical(&canonical).is_some() {
            return Ok(self
                .substitute_type_with_control(declared_type_name, control)?
                .map(|ty| canonical_column_type_name_with_control(&ty, control))
                .transpose()?);
        }
        Ok(Some(canonical))
    }

    /// Publish an independently admitted invocation binding.
    pub fn invocation_binding_with_control(
        &self,
        declared_return_type: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoutineInvocationBinding>, SQLError> {
        self.invocation_binding_parts_with_control(declared_return_type, control)?
            .finish(control)
    }

    /// Publish an independent invocation binding, retaining candidate scratch until every destination is admitted.
    pub(crate) fn invocation_binding_parts_with_control(
        &self,
        declared_return_type: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<RoutineInvocationParts, SQLError> {
        control.check()?;
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
        let mut positions = ProductionVec::new(*control);
        positions.extend_from_slice(&self.argument_positions)?;
        let mut targets = ProductionVec::new(*control);
        targets.reserve(self.coercion_targets.len())?;
        for target in &self.coercion_targets {
            targets.push_produced(match &target.column_type {
                Some(ty) => ty.catalog_name_with_control(control)?,
                None => control.copy_text(&target.type_name)?,
            })?;
        }
        let mut sources = ProductionVec::new(*control);
        sources.reserve(self.argument_sources.len())?;
        for source in &self.argument_sources {
            let value = source
                .as_ref()
                .map(|name| -> Result<Produced<String>, SQLError> {
                    match parse_type(name, control)? {
                        Some(ty) => Ok(ty.catalog_name_with_control(control)?),
                        None => Ok(control.copy_text(name)?),
                    }
                })
                .transpose()?;
            let (value, memory) = match value {
                Some(value) => {
                    let (value, memory) = value.into_parts();
                    (Some(value), memory)
                }
                None => (None, control.empty_reservation()),
            };
            sources.push_produced(control.finish(value, memory)?)?;
        }
        let mut parameter_types = ProductionVec::new(*control);
        parameter_types.reserve(self.parameter_types.len())?;
        for (name, ty) in self.parameter_types.iter().zip(&self.parameter_type_values) {
            parameter_types.push_produced(match ty {
                Some(ty) => ty.catalog_name_with_control(control)?,
                None => control.copy_text(name)?,
            })?;
        }
        let return_type = declared_return_type
            .map(|name| -> Result<Produced<String>, SQLError> {
                match self.substitute_type_with_control(name, control)? {
                    Some(ty) => Ok(ty.catalog_name_with_control(control)?),
                    None => Ok(control.copy_text(name)?),
                }
            })
            .transpose()?;
        Ok(RoutineInvocationParts {
            argument_positions: positions.finish()?,
            argument_targets: targets.finish()?,
            argument_sources: sources.finish()?,
            parameter_types: parameter_types.finish()?,
            return_type,
            variadic_mode,
        })
    }
}

pub(super) fn parse_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error) if resource_failure(&error) => Err(error),
        Err(_) => Ok(None),
    }
}

pub(crate) struct RoutineInvocationParts {
    pub(crate) argument_positions: Produced<Vec<usize>>,
    pub(crate) argument_targets: Produced<Vec<String>>,
    pub(crate) argument_sources: Produced<Vec<Option<String>>>,
    pub(crate) parameter_types: Produced<Vec<String>>,
    pub(crate) return_type: Option<Produced<String>>,
    pub(crate) variadic_mode: InvocationVariadicMode,
}

impl RoutineInvocationParts {
    pub(crate) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoutineInvocationBinding>, SQLError> {
        let mut fields = Components::new(*control);
        let binding = RoutineInvocationBinding {
            argument_positions: fields.take(self.argument_positions),
            argument_targets: fields.take(self.argument_targets),
            argument_sources: fields.take(self.argument_sources),
            parameter_types: fields.take(self.parameter_types),
            return_type: self.return_type.map(|value| fields.take(value)),
            variadic_mode: self.variadic_mode,
        };
        fields.finish(binding)
    }
}
