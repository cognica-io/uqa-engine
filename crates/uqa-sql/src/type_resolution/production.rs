//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained results at custom resolver boundaries; native producers use the same payload constructors directly.

use super::{ColumnType, ResolvedFunctionOverload, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn external_type(
    value: Option<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    value
        .map(|value| {
            if control.budget().is_none() {
                Ok(control.finish(value, None)?)
            } else {
                value.clone_with_control(control).map_err(Into::into)
            }
        })
        .transpose()
}

pub(super) fn external_overload(
    value: Option<ResolvedFunctionOverload>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
    value
        .map(|value| {
            if control.budget().is_none() {
                Ok(control.finish(value, None)?)
            } else {
                value.clone_with_control(control)
            }
        })
        .transpose()
}

impl ResolvedFunctionOverload {
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let binding = self.binding.clone_with_control(control)?;
        let result = self.return_type.clone_with_control(control)?;
        let (binding, binding_memory) = binding.into_parts();
        let (return_type, result_memory) = result.into_parts();
        Ok(control.finish(
            Self {
                binding,
                return_type,
                exact_matches: self.exact_matches,
                known_arguments: self.known_arguments,
                preferred_matches: self.preferred_matches,
                precedes_pg_catalog: self.precedes_pg_catalog,
            },
            control.combine(binding_memory, result_memory),
        )?)
    }
}

impl super::BuiltinFunctionOverload {
    pub(crate) fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let name = control.copy_text(&self.name)?;
        let mut names = uqa_core::memory::ProductionVec::new(*control);
        let mut types = uqa_core::memory::ProductionVec::new(*control);
        names.reserve(self.argument_names.len())?;
        types.reserve(self.argument_types.len())?;
        for name in &self.argument_names {
            let name = name
                .as_deref()
                .map(|name| control.copy_text(name))
                .transpose()?;
            let (name, memory) = name.map_or((None, control.empty_reservation()), |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            });
            names.push_produced(control.finish(name, memory)?)?;
        }
        for ty in &self.argument_types {
            types.push_produced(ty.clone_with_control(control)?)?;
        }
        let names = names.finish()?;
        let types = types.finish()?;
        let result = self.return_type.clone_with_control(control)?;
        let (name, name_memory) = name.into_parts();
        let (argument_names, labels_memory) = names.into_parts();
        let (argument_types, types_memory) = types.into_parts();
        let (return_type, result_memory) = result.into_parts();
        Ok(control.finish(
            Self {
                variadic: self.variadic,
                name,
                argument_names,
                argument_types,
                default_arguments: self.default_arguments,
                return_type,
            },
            control.combine(
                name_memory,
                control.combine(labels_memory, control.combine(types_memory, result_memory)),
            ),
        )?)
    }
}
