//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{fmt, sync::Arc};
use uqa_core::{
    image_io::ImageIoError,
    memory::{MemoryError, Produced, ProductionControl},
    ValueRetentionError,
};

#[derive(Debug, thiserror::Error)]
pub enum RoutineRestoreInputError {
    #[error(transparent)]
    Native(#[from] ValueRetentionError),
    #[error(transparent)]
    Work(#[from] ImageIoError),
    #[error(transparent)]
    Diagnostic(#[from] RoutineRestoreDiagnostic),
}

impl From<MemoryError> for RoutineRestoreInputError {
    fn from(error: MemoryError) -> Self {
        Self::Native(error.into())
    }
}

impl From<crate::type_resolution::RoutineSignatureInputError> for RoutineRestoreInputError {
    fn from(error: crate::type_resolution::RoutineSignatureInputError) -> Self {
        match error {
            crate::type_resolution::RoutineSignatureInputError::Native(error) => {
                Self::Native(error)
            }
            crate::type_resolution::RoutineSignatureInputError::Work(error) => Self::Work(error),
        }
    }
}

#[derive(Debug)]
enum Detail {
    Message(Produced<String>),
    Duplicate {
        name: Produced<String>,
        signature: Produced<Vec<String>>,
    },
}

/// The diagnostic owns its native message or canonical signature inputs after the failed candidate is retired. Formatting uses the established public signature spelling.
#[derive(Debug)]
pub struct RoutineRestoreDiagnostic {
    detail: Detail,
    header: Option<uqa_core::memory::MemoryReservation>,
}

impl RoutineRestoreDiagnostic {
    pub(super) fn message(message: Produced<String>) -> Self {
        Self {
            detail: Detail::Message(message),
            header: None,
        }
    }
    pub(super) fn duplicate(name: Produced<String>, signature: Produced<Vec<String>>) -> Self {
        Self {
            detail: Detail::Duplicate { name, signature },
            header: None,
        }
    }

    pub fn into_shared(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, ValueRetentionError> {
        self.header = control.reserve(size_of::<Self>())?;
        Ok(Arc::new(self))
    }
}

impl fmt::Display for RoutineRestoreDiagnostic {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Detail::Message(message) => output.write_str(message),
            Detail::Duplicate { name, signature } => write!(
                output,
                "duplicate persisted routine identity `{}`",
                crate::routines::lifecycle::routine_signature_label(name, signature)
            ),
        }
    }
}
impl std::error::Error for RoutineRestoreDiagnostic {}
