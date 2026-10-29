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
