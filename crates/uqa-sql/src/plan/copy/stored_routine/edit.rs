//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    capture, Field, FunctionBody, FunctionParam, MemoryError, MemoryReservation, ProductionControl,
    Result, Statement, StoredRoutineDefinition,
};
use crate::{catalog::work::Work, plan::copy::syntax::native::Field as SyntaxField};

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

struct Editing<'a, T, M> {
    target: Target<'a, T, M>,
    scratch: T,
    _scratch_memory: Option<MemoryReservation>,
}

impl<'a, T: SyntaxField, M> Target<'a, T, M> {
    async fn select(
        self,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Editing<'a, T, M>> {
        let scratch = capture::from(self.value, control, work).await?;
        let (scratch, memory) = scratch.value.into_parts();
        Ok(Editing {
            target: self,
            scratch,
            _scratch_memory: memory,
        })
    }
}

impl<T: SyntaxField, M> Editing<'_, T, M> {
    async fn finish(
        self,
        changed: bool,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
        merge_targets: impl FnOnce(bool) -> M,
    ) -> Result<bool> {
        control.check()?;
        if !changed {
            return Ok(false);
        }
        let field = capture::from(&self.scratch, control, work).await?;
        self.target.replace(Field::from_produced(
            field.value,
            field.children,
            merge_targets(field.has_merge_targets),
        ))
    }
}

macro_rules! editable {
    ($edit:ident, $ty:ty, $metadata:ty, $merge_targets:expr) => {
        /// A private edit retains the selected syntax until its changed fields can be published together.
        pub struct $edit<'a>(Editing<'a, $ty, $metadata>);

        impl $edit<'_> {
            pub fn value_mut(&mut self) -> &mut $ty {
                &mut self.0.scratch
            }

            pub async fn finish_from(
                self,
                changed: bool,
                control: &ProductionControl<'_>,
                work: &mut (impl Work + Send),
            ) -> Result<bool> {
                self.0.finish(changed, control, work, $merge_targets).await
            }
        }
    };
}

editable!(StoredRoutineParameterEdit, FunctionParam, (), |_| ());
editable!(
    StoredRoutineStatementEdit,
    Statement,
    bool,
    std::convert::identity
);

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
        let edit = Target {
            value,
            field: &mut self.parameters[position],
            direct: &mut self.direct,
            children: &mut self.children,
        }
        .select(control, work)
        .await?;
        Ok(Some(StoredRoutineParameterEdit(edit)))
    }

    /// Select the current SQL-standard body statement and retain its native field and MERGE ownership together.
    pub async fn statement_edit_from(
        &mut self,
        position: usize,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Option<StoredRoutineStatementEdit<'_>>> {
        let FunctionBody::Statements(body) = &mut self.definition.body else {
            return Ok(None);
        };
        let Some(value) = body.get_mut(position) else {
            return Ok(None);
        };
        let edit = Target {
            value,
            field: &mut self.statements[position],
            direct: &mut self.direct,
            children: &mut self.children,
        }
        .select(control, work)
        .await?;
        Ok(Some(StoredRoutineStatementEdit(edit)))
    }
}
