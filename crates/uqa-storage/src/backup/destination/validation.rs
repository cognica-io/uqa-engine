//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An admitted validation continuation owns every private provider handle until terminal retirement.

use super::{BackupError, BackupResult, Budgeted, StorageReadControl};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupImageValidationStep {
    Pending,
    Complete,
}

/// One logical validation owner. An advance performs at most its supplied number of owner-defined steps; the owner retains all source handles until completion or failure.
pub trait BackupImageValidationWork {
    fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep>;
}

pub struct OwnedBackupImageValidation {
    state: State,
}

enum State {
    Active(Input),
    Complete,
    Failed,
}

struct Input {
    work: Budgeted<Box<dyn BackupImageValidationWork>>,
    control: StorageReadControl,
}

impl OwnedBackupImageValidation {
    /// Admit the concrete continuation before its constructor can open or retain provider resources.
    pub fn create<T: BackupImageValidationWork + 'static>(
        control: &StorageReadControl,
        create: impl FnOnce() -> BackupResult<T>,
    ) -> BackupResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<T>())?;
        let work = create()?;
        control.check()?;
        Ok(Self {
            state: State::Active(Input {
                work: Budgeted::new(Box::new(work), memory),
                control: control.clone(),
            }),
        })
    }

    /// Represent a callback that has already finished and closed all of its private provider handles.
    pub fn completed(control: &StorageReadControl) -> BackupResult<Self> {
        control.check()?;
        Ok(Self {
            state: State::Complete,
        })
    }

    /// Preserve the original control and continuation between calls. Errors and unwinds retire all retained resources and permanently close the owner; completion is idempotent after late cancellation.
    pub fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep> {
        match self.state {
            State::Complete => return Ok(BackupImageValidationStep::Complete),
            State::Failed => return Err(BackupError::Finished),
            State::Active(_) => (),
        }
        let State::Active(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        if maximum_steps == 0 {
            return Err(BackupError::Invalid("zero validation step limit"));
        }
        input.control.check()?;
        let step = input.work.as_mut().advance(maximum_steps)?;
        input.control.check()?;
        self.state = match step {
            BackupImageValidationStep::Pending => State::Active(input),
            BackupImageValidationStep::Complete => {
                drop(input);
                State::Complete
            }
        };
        Ok(step)
    }
}

#[cfg(test)]
mod tests;
