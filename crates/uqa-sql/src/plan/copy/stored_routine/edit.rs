//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    parameter, Field, FunctionParam, MemoryError, MemoryReservation, ProductionControl, Result,
    StoredRoutineDefinition,
};
use crate::catalog::work::Work;

/// A receipt and its value are replaced together only after the new stored projection is complete.
pub(super) struct Target<'a, T, M> {
    pub(super) value: &'a mut T,
    pub(super) field: &'a mut Field<M>,
    pub(super) direct: &'a mut MemoryReservation,
    pub(super) children: &'a mut usize,
}

impl<T, M> Target<'_, T, M> {
    pub(super) fn replace(
        self,
        (value, lease, next): (T, MemoryReservation, Field<M>),
    ) -> Result<bool> {
        let updated = (*self.children - self.field.children)
            .checked_add(next.children)
            .ok_or(MemoryError::SizeOverflow)?;
        *self.value = value;
        drop(self.direct.split(self.field.direct));
        self.direct.absorb(lease);
        *self.children = updated;
        *self.field = next;
        Ok(true)
    }
}

/// A private editable parameter. Dropping the edit leaves the declaration and its accounting intact.
pub struct StoredRoutineParameterEdit<'a> {
    target: Target<'a, FunctionParam, ()>,
    scratch: FunctionParam,
    _scratch_memory: Option<MemoryReservation>,
}

impl StoredRoutineDefinition {
    /// Select the current parameter, including prior catalog migrations, under the original field allowance.
    pub async fn parameter_edit_from(
        &mut self,
        position: usize,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Option<StoredRoutineParameterEdit<'_>>> {
        let Some(value) = self.definition.params.get_mut(position) else {
            return Ok(None);
        };
        let (scratch, _) = parameter::capture_from(value, control, work).await?;
        let (scratch, memory) = scratch.into_parts();
        Ok(Some(StoredRoutineParameterEdit {
            target: Target {
                value,
                field: &mut self.parameters[position],
                direct: &mut self.direct,
                children: &mut self.children,
            },
            scratch,
            _scratch_memory: memory,
        }))
    }
}

impl StoredRoutineParameterEdit<'_> {
    pub fn value_mut(&mut self) -> &mut FunctionParam {
        &mut self.scratch
    }

    /// Reproject every changed native field before replacing its original in the declaration.
    pub async fn finish_from(
        self,
        changed: bool,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<bool> {
        control.check()?;
        if !changed {
            return Ok(false);
        }
        let (value, children) = parameter::capture_from(&self.scratch, control, work).await?;
        self.target
            .replace(Field::from_produced(value, children, ()))
    }
}
