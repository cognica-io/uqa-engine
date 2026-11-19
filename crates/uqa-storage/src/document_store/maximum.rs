//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The maximum visible document identity retains one provider boundary until completion.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{memory::Budgeted, DocId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaximumDocIdStep {
    Pending,
    Complete(DocId),
}

/// A provider consumes at most one selected record slot without reading document values. It checks its original read control as well as the supplied input control; a deleted slot still consumes a step.
pub trait MaximumDocIdSource: Send {
    fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<MaximumDocIdStep>;
}

pub struct MaximumDocIdInput {
    state: State,
    control: StorageReadControl,
}

enum State {
    Reading(Budgeted<Box<dyn MaximumDocIdSource>>),
    Complete(DocId),
    Failed,
}

pub fn validate_limit(
    maximum_key_bytes: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    if maximum_key_bytes == 0 {
        return Err(StorageBackendError::Other(
            "zero document maximum key limit".into(),
        ));
    }
    Ok(())
}

impl MaximumDocIdInput {
    /// Adopt a provider's selected boundary, admitting the concrete source before boxing it.
    pub fn new<T: MaximumDocIdSource + 'static>(
        source: T,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<T>())?;
        let source: Box<dyn MaximumDocIdSource> = Box::new(source);
        let source = Budgeted::new(source, memory);
        control.check()?;
        Ok(Self {
            state: State::Reading(source),
            control: control.clone(),
        })
    }

    /// An absent physical owner has no visible documents and needs no retained source.
    pub fn empty(control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            state: State::Complete(0),
            control: control.clone(),
        })
    }

    /// Completion releases the source before publishing its scalar result and is idempotent. Error, cancellation and unwind retire the entire unfinished owner; failed input cannot resume.
    pub fn advance(&mut self) -> StorageBackendResult<MaximumDocIdStep> {
        match self.state {
            State::Complete(value) => return Ok(MaximumDocIdStep::Complete(value)),
            State::Failed => {
                return Err(StorageBackendError::Other(
                    "document maximum input is finished".into(),
                ));
            }
            State::Reading(_) => (),
        }
        let State::Reading(mut source) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        self.control.check()?;
        let step = source.as_mut().advance(&self.control)?;
        self.control.check()?;
        self.state = match step {
            MaximumDocIdStep::Pending => State::Reading(source),
            MaximumDocIdStep::Complete(value) => {
                drop(source);
                State::Complete(value)
            }
        };
        Ok(step)
    }
}

#[cfg(test)]
mod tests;
