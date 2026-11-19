//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::memory::Budgeted;

/// Exact pre-output progress; positions include visited metadata entries and continuation boundaries, not physical I/O estimates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePreparation {
    pub positions: usize,
    pub ready: bool,
}

/// An owning codec's resumable image source. A nonempty read returns zero only at EOF; errors make the partial image unusable.
pub trait ImageRead: Send {
    /// Advance pre-output validation by at most this many owner-defined positions. A pending result must visit at least one position; after readiness, preparation remains ready with zero further positions. Every call preserves the source's cancellation checks.
    fn prepare(&mut self, _maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        Ok(ImagePreparation {
            positions: 0,
            ready: true,
        })
    }

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
    prepared: bool,
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
            prepared: false,
            control: control.clone(),
        };
        control.check()?;
        Ok(reader)
    }

    /// Keep the captured source's cancellation active during preparation and output, independently of the invoking allowance.
    pub fn with_source_control(
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

    /// Perform bounded pre-output work. An active cursor requires a positive limit; readiness permits reads but does not complete the image. Errors and unwinds retire the cursor just as failed reads do.
    pub fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Complete => {
                self.state = State::Complete;
                Ok(ImagePreparation {
                    positions: 0,
                    ready: true,
                })
            }
            State::Failed => Err(StorageBackendError::Other("failed image reader".into())),
            State::Active(source) => {
                let mut source = source.into_parts();
                self.control.check()?;
                if maximum_positions == 0 {
                    return Err(StorageBackendError::Other(
                        "image preparation requires a positive position limit".into(),
                    ));
                }
                let progress = source.0.prepare(maximum_positions)?;
                if progress.positions > maximum_positions
                    || (!progress.ready && progress.positions == 0)
                    || (self.prepared && progress.positions != 0)
                {
                    return Err(StorageBackendError::Other(
                        "invalid image preparation position count".into(),
                    ));
                }
                self.prepared = progress.ready;
                self.control.check()?;
                self.state = State::Active(Budgeted::new(source.0, source.1));
                Ok(progress)
            }
        }
    }

    /// Read at most 16 KiB, completing any remaining preparation synchronously. Scheduled callers first drive `prepare` with their position allowance. Empty buffers do not advance or complete an active source; a completed source remains at EOF after later cancellation.
    pub fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        if !output.is_empty() {
            while !self.prepare(128)?.ready {}
        }
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
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        self.original.check()?;
        let ready = self.source.prepare(maximum_positions)?;
        self.original.check()?;
        Ok(ready)
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.original.check()?;
        let count = self.source.read(output)?;
        self.original.check()?;
        Ok(count)
    }
}
