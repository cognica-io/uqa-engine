//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared immutable CTE declarations retain the admission of their copied payloads.

use super::CtePlan;
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, Clone)]
pub struct RetainedCtePlan(Arc<Produced<CtePlan>>);

impl RetainedCtePlan {
    pub fn capture(plan: &CtePlan, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Self::from_produced(plan.clone_with_control(control)?, control)
    }

    /// Retain an admitted declaration after preparation has attached shared analysis metadata. The Arc payload is admitted separately; reference-count bookkeeping follows the common shared-owner contract.
    pub fn from_produced(
        plan: Produced<CtePlan>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let wrapper = control.reserve(size_of::<Produced<CtePlan>>())?;
        let (plan, memory) = plan.into_parts();
        let output = control.finish(plan, control.combine(memory, wrapper))?;
        Ok(Self(Arc::new(output)))
    }
}

impl From<CtePlan> for RetainedCtePlan {
    fn from(plan: CtePlan) -> Self {
        Self(Arc::new(
            ProductionControl::uncontrolled()
                .finish(plan, None)
                .expect("uncontrolled CTE ownership"),
        ))
    }
}

impl AsRef<CtePlan> for RetainedCtePlan {
    fn as_ref(&self) -> &CtePlan {
        &self.0
    }
}

impl std::ops::Deref for RetainedCtePlan {
    type Target = CtePlan;
    fn deref(&self) -> &CtePlan {
        self.as_ref()
    }
}
