//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::memory::Budgeted;

/// An owning codec's resumable image source. A nonempty read returns zero only at EOF; errors make the partial image unusable.
pub trait ImageRead: Send {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize>;
}

enum State<'a> {
    Active(Budgeted<Box<dyn ImageRead + 'a>>),
    Complete,
    Failed,
}

/// An admitted image cursor. Its allocation is charged before boxing, and completion, failure or unwind releases the source and its allowance. Failed readers cannot resume. Any borrowed source generation must outlive this handle.
pub struct ImageReader<'a> {
    state: State<'a>,
    control: StorageReadControl,
}

impl<'a> ImageReader<'a> {
    pub fn new<R: ImageRead + 'a>(
        source: R,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<R>())?;
        let source: Box<dyn ImageRead + 'a> = Box::new(source);
        let reader = Self {
            state: State::Active(Budgeted::new(source, memory)),
            control: control.clone(),
        };
        control.check()?;
        Ok(reader)
    }

    pub(crate) fn with_source_control(
        self,
        original: Option<&'a StorageReadControl>,
        invoking: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        match original {
            Some(original) => {
                original.check()?;
                Self::new(
                    RetainedRead {
                        source: self,
                        original,
                    },
                    invoking,
                )
            }
            None => Ok(self),
        }
    }

    /// Read at most 16 KiB. Empty buffers do not complete an active source; a completed source remains at EOF after later cancellation.
    pub fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Complete => {
                self.state = State::Complete;
                Ok(0)
            }
            State::Failed => Err(StorageBackendError::Other("failed image reader".into())),
            State::Active(source) => {
                let mut source = source.into_parts();
                self.control.check()?;
                let limit = output.len().min(16 * 1024);
                let count = source.0.read(&mut output[..limit])?;
                self.control.check()?;
                if count > limit {
                    return Err(StorageBackendError::Other(
                        "invalid image reader byte count".into(),
                    ));
                }
                self.state = if count == 0 && limit != 0 {
                    State::Complete
                } else {
                    State::Active(Budgeted::new(source.0, source.1))
                };
                Ok(count)
            }
        }
    }
}

struct RetainedRead<'a> {
    source: ImageReader<'a>,
    original: &'a StorageReadControl,
}

impl ImageRead for RetainedRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.original.check()?;
        let count = self.source.read(output)?;
        self.original.check()?;
        Ok(count)
    }
}
