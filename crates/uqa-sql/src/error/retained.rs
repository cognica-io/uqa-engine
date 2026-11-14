//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::SQLError;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// One immutable routine diagnostic and its original payload/header allowance. The primary error is always an unpositioned Routine variant; outer context and positions keep their established ownership.
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
    pub(crate) fn admit_routine_message(
        sqlstate: &'static str,
        message: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, Self> {
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
        Ok(Self::RetainedRoutine(std::sync::Arc::new(
            RetainedRoutineDiagnostic {
                error: Self::Routine { sqlstate, message },
                _memory: memory,
            },
        )))
    }
}

#[cfg(test)]
mod tests;
