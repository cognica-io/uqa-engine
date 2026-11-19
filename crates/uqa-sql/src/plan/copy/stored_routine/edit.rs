//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    capture, Field, FunctionBody, FunctionParam, MemoryError, MemoryReservation, ProductionControl,
    Result, Statement, StoredRoutineDefinition,
};
use crate::{
    catalog::{image::StoredSyntaxRetirement, work::Work},
    plan::copy::syntax::native::Field as SyntaxField,
};

/// A committed replacement can retire its previous value before selecting the next field.
pub(super) type Displaced = (StoredSyntaxRetirement, MemoryReservation);

/// A receipt and its value are replaced together only after the new stored projection is complete.
pub(super) struct Target<'a, T, M> {
    pub(super) value: &'a mut T,
    pub(super) field: &'a mut Field<M>,
    pub(super) direct: &'a mut MemoryReservation,
    pub(super) children: &'a mut usize,
}

impl<T, M> Target<'_, T, M> {
    pub(super) fn replace(self, field: (T, MemoryReservation, Field<M>)) -> Result<bool> {
        self.exchange(field).map(|_| true)
    }
    fn exchange(
        self,
        (value, lease, next): (T, MemoryReservation, Field<M>),
    ) -> Result<(T, MemoryReservation)> {
        let updated = (*self.children - self.field.children)
            .checked_add(next.children)
            .ok_or(MemoryError::SizeOverflow)?;
        let previous = std::mem::replace(self.value, value);
        let memory = self.direct.split(self.field.direct);
        self.direct.absorb(lease);
        *self.children = updated;
        *self.field = next;
        Ok((previous, memory))
    }
}

struct Editing<'a, T, M> {
    target: Target<'a, T, M>,
    scratch: T,
    _scratch_memory: Option<MemoryReservation>,
    original_depth: usize,
    retirement: Option<StoredSyntaxRetirement>,
    displaced: &'a mut Option<Displaced>,
}

impl<'a, T: SyntaxField, M> Target<'a, T, M> {
    async fn select<W: Work + Send>(
        self,
        displaced: &'a mut Option<Displaced>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Editing<'a, T, M>> {
        let scratch = capture::from(self.value, control, work).await?;
        let original_depth = if W::MAY_SUSPEND {
            scratch.retirement_depth()
        } else {
            0
        };
        let mut retirement = None;
        StoredSyntaxRetirement::prepare(&mut retirement, original_depth, self.direct.budget())?;
        let (scratch, memory) = scratch.value.into_parts();
        Ok(Editing {
            target: self,
            scratch,
            _scratch_memory: memory,
            original_depth,
            retirement,
            displaced,
        })
    }
}

impl<T: SyntaxField, M> Editing<'_, T, M> {
    async fn finish<W: Work + Send>(
        mut self,
        changed: bool,
        control: &ProductionControl<'_>,
        work: &mut W,
        merge_targets: impl FnOnce(bool) -> M,
        retire: fn(&mut StoredSyntaxRetirement, T),
    ) -> Result<bool> {
        control.check()?;
        let field = if changed {
            let captured = capture::from(&self.scratch, control, work).await?;
            if W::MAY_SUSPEND {
                StoredSyntaxRetirement::prepare(
                    &mut self.retirement,
                    captured.retirement_depth(),
                    self.target.direct.budget(),
                )?;
            }
            Some(Field::from_produced(
                captured.value,
                captured.children,
                merge_targets(captured.has_merge_targets),
            ))
        } else {
            None
        };
        if let Some(workspace) = &mut self.retirement {
            retire(workspace, self.scratch);
            workspace.finish(control, work, changed).await?;
        }
        let Some(field) = field else { return Ok(false) };
        let (previous, memory) = self.target.exchange(field)?;
        if self.original_depth != 0 {
            let mut workspace = self.retirement.expect("original syntax depth");
            retire(&mut workspace, previous);
            *self.displaced = Some((workspace, memory));
        }
        Ok(true)
    }
}

macro_rules! editable {
    ($edit:ident, $ty:ty, $metadata:ty, $merge_targets:expr, $retire:ident) => {
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
                self.0
                    .finish(
                        changed,
                        control,
                        work,
                        $merge_targets,
                        StoredSyntaxRetirement::$retire,
                    )
                    .await
            }
        }
    };
}

editable!(
    StoredRoutineParameterEdit,
    FunctionParam,
    (),
    |_| (),
    parameter
);
editable!(
    StoredRoutineStatementEdit,
    Statement,
    bool,
    std::convert::identity,
    statement
);

impl StoredRoutineDefinition {
    async fn retire_displaced(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<()> {
        if let Some((tree, _memory)) = &mut self.displaced {
            tree.finish(control, work, true).await?;
            self.displaced = None;
        }
        Ok(())
    }

    /// Select the current parameter, including prior catalog migrations, under the original field allowance.
    pub async fn parameter_edit_from(
        &mut self,
        position: usize,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Option<StoredRoutineParameterEdit<'_>>> {
        self.retire_displaced(control, work).await?;
        let Some(value) = self.definition.params.get_mut(position) else {
            return Ok(None);
        };
        let edit = Target {
            value,
            field: &mut self.parameters[position],
            direct: &mut self.direct,
            children: &mut self.children,
        }
        .select(&mut self.displaced, control, work)
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
        self.retire_displaced(control, work).await?;
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
        .select(&mut self.displaced, control, work)
        .await?;
        Ok(Some(StoredRoutineStatementEdit(edit)))
    }
}
