//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::SQLError;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// One immutable SQL diagnostic and its original payload/header allowance. Outer context and positions retain the original primary error.
#[derive(Debug)]
pub struct RetainedRoutineDiagnostic {
    pub(super) error: SQLError,
    _memory: MemoryReservation,
}

impl std::fmt::Display for RetainedRoutineDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for RetainedRoutineDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl SQLError {
    /// Preserve an already produced SQL diagnostic and its SQLSTATE without formatting or copying its message again.
    pub fn admit_routine_message(
        sqlstate: &'static str,
        message: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, Self> {
        Ok(Self::produced_routine_message(sqlstate, message, control)?)
    }

    pub(crate) fn admit_internal_message(
        message: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, Self> {
        let (message, memory) = message.into_parts();
        let error = Self::Internal(message);
        let Some(mut memory) = memory else {
            return Ok(error);
        };
        memory.absorb(
            memory
                .budget()
                .reserve(size_of::<RetainedRoutineDiagnostic>())?,
        );
        control.check()?;
        Ok(error.with_diagnostic_memory(memory))
    }

    fn with_diagnostic_memory(self, memory: MemoryReservation) -> Self {
        Self::RetainedRoutine(std::sync::Arc::new(RetainedRoutineDiagnostic {
            error: self,
            _memory: memory,
        }))
    }

    /// Preserve the SQLSTATE and display text when a native catalog returns an analysis diagnostic across the storage boundary.
    pub fn into_retained_catalog_message(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, uqa_core::ValueRetentionError> {
        if control.budget().is_none()
            || matches!(self, Self::Cancelled(_) | Self::RetainedRoutine(_))
        {
            return Ok(self);
        }
        let Some(sqlstate) = self.sqlstate() else {
            return Ok(self);
        };
        Self::produced_routine_message(sqlstate, control.format(format_args!("{self}"))?, control)
    }

    fn produced_routine_message(
        sqlstate: &str,
        message: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, uqa_core::ValueRetentionError> {
        if control.budget().is_none() {
            let (message, memory) = message.into_parts();
            debug_assert!(memory.is_none());
            return Ok(Self::Routine {
                sqlstate: sqlstate.into(),
                message,
            });
        }
        let header = control.reserve(size_of::<RetainedRoutineDiagnostic>())?;
        let state = control.copy_text(sqlstate)?;
        control.check()?;
        let (sqlstate, state_memory) = state.into_parts();
        let (message, message_memory) = message.into_parts();
        let memory = control
            .combine(control.combine(header, state_memory), message_memory)
            .expect("admitted routine diagnostic");
        Ok(Self::Routine { sqlstate, message }.with_diagnostic_memory(memory))
    }
}

#[cfg(test)]
mod tests;
