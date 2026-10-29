//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable statement roots publish only after their independent child edits finish.

use super::{MutableCommandPlan, MutableQueryPlan};
use crate::{
    plan::{CommandPlan, OwnedExecutablePlan, OwnedQueryPlan, PlanRef, UnifiedPlan},
    SQLError,
};
use uqa_core::memory::ProductionControl;

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "statement owners remain inline so moving an admitted root requires no additional allocation"
)]
pub enum MutableExecutablePlan {
    Query(MutableQueryPlan),
    Command(MutableCommandPlan),
}

impl UnifiedPlan {
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError> {
        PlanRef::from(self).copy_for_rewrite(control)
    }
}

impl PlanRef<'_> {
    pub fn copy_for_rewrite(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError> {
        Ok(match self {
            Self::Query(query) => MutableExecutablePlan::Query(query.copy_for_rewrite(control)?),
            Self::Command(command) => {
                MutableExecutablePlan::Command(command.copy_for_rewrite(control)?)
            }
        })
    }
}

impl MutableExecutablePlan {
    pub fn as_plan_ref(&self) -> PlanRef<'_> {
        match self {
            Self::Query(query) => PlanRef::Query(query),
            Self::Command(command) => PlanRef::Command(command),
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        match self {
            Self::Query(query) => query.reserved_bytes(),
            Self::Command(command) => command.reserved_bytes(),
        }
    }

    /// Admit the final statement box and transfer only surviving constructor leases into immutable execution storage.
    pub fn finish(self, control: &ProductionControl<'_>) -> Result<OwnedExecutablePlan, SQLError> {
        match self {
            Self::Query(query) => Ok(OwnedQueryPlan::from_produced(
                query.finish(control)?,
                control,
            )?
            .into_executable()),
            Self::Command(command) => {
                let root = control.reserve(size_of::<CommandPlan>())?;
                let (command, memory) = command.finish(control)?.into_parts();
                let memory = control.combine(memory, root);
                Ok(OwnedExecutablePlan::from(
                    control.finish(UnifiedPlan::Command(Box::new(command)), memory)?,
                ))
            }
        }
    }
}
