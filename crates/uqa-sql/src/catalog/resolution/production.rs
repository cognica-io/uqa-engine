//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session captures admit each owned namespace buffer before copying borrowed state.

use super::{RelationLookupMode, RelationNameResolution, RoleReference};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl RelationNameResolution {
    /// Capture already expanded search-path names and a separately admitted temporary name. Bound roles retain their immutable incarnation without copying its name.
    pub fn from_inputs_with_control<'a>(
        search_path: impl IntoIterator<Item = &'a str>,
        temporary_schema: Produced<String>,
        temporary_namespace_allocated: bool,
        current_user: &RoleReference,
        lookup_mode: RelationLookupMode,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        let mut path = ProductionVec::new(*control);
        for name in search_path {
            path.push_produced(control.copy_text(name)?)?;
        }
        let role = match current_user {
            RoleReference::Named(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                control.finish(RoleReference::Named(name), memory)?
            }
            RoleReference::Bound(binding) => control.finish(
                RoleReference::Bound(binding.clone()),
                control.empty_reservation(),
            )?,
        };
        let (search_path, path_memory) = path.finish()?.into_parts();
        let (temporary_schema, temporary_memory) = temporary_schema.into_parts();
        let (current_user, role_memory) = role.into_parts();
        Ok(control.finish(
            Self {
                search_path,
                temporary_schema,
                temporary_namespace_allocated,
                current_user,
                lookup_mode,
            },
            control.combine(control.combine(path_memory, temporary_memory), role_memory),
        )?)
    }

    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        Self::from_inputs_with_control(
            self.search_path.iter().map(String::as_str),
            control.copy_text(&self.temporary_schema)?,
            self.temporary_namespace_allocated,
            &self.current_user,
            self.lookup_mode,
            control,
        )
    }
}

#[cfg(test)]
mod tests;
