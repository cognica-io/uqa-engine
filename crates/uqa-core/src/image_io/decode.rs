//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative native decoding retains the owning Rust continuation without a worker or external executor.

use super::{invalid, ImageIoError, ImageIoResult, CHUNK};
use crate::{
    memory::{Budgeted, MemoryBudget, RetainedValue},
    CancellationToken,
};
use parking_lot::Mutex;
use std::{
    future::Future,
    io::Read,
    pin::Pin,
    task::{Context, Poll, Waker},
};

mod input;
pub use input::AsyncImageInput;
mod source;
pub use source::{decode_image_from, AsyncImageRead};
#[cfg(test)]
mod tests;

/// Independent byte and continuation-poll ceilings for one advance. Both must be nonzero; each read attempt, including an interrupted read, consumes a step. Codec owners must insert checkpoints to bound computation between polls.
#[derive(Clone, Copy, Debug)]
pub struct ImageDecodeWindow {
    pub bytes: usize,
    pub steps: usize,
}

enum Phase {
    Idle,
    Read(usize),
    Bytes(usize),
    Checkpoint,
    Continue,
}

struct Channel {
    buffer: [u8; CHUNK],
    phase: Phase,
}

type Shared = RetainedValue<Mutex<Channel>>;
type Decoder<T, E> = Budgeted<Pin<Box<dyn Future<Output = Result<T, E>> + Send>>>;

struct State<T, E> {
    future: Decoder<T, E>,
    channel: Shared,
    cancellation: CancellationToken,
}

/// Own an admitted image continuation and a fixed read window. The codec may await image reads and explicit checkpoints, but never an external asynchronous operation. No reader is retained between calls, no bytes are replayed and completed output belongs to its native owner.
pub struct ImageDecoder<T, E = ImageIoError> {
    state: Option<State<T, E>>,
}

/// The codec's retained input context. Borrow an `AsyncImageInput` inside the owning future; every primitive resumes through the same fixed channel.
pub struct ImageRead {
    channel: Shared,
    memory: MemoryBudget,
    cancellation: CancellationToken,
}

impl ImageRead {
    pub fn input(&mut self) -> AsyncImageInput<'_> {
        AsyncImageInput {
            channel: &self.channel,
            memory: &self.memory,
            cancellation: &self.cancellation,
        }
    }
}

impl<T, E: From<ImageIoError>> ImageDecoder<T, E> {
    /// Admit the original future and input buffer before invoking the constructor or allocating either shared owner. A constructor failure or unwind returns both reservations.
    pub fn new<F>(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
        create: impl FnOnce(ImageRead) -> F,
    ) -> ImageIoResult<Self>
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        cancellation.check()?;
        let header = memory.reserve(size_of::<F>())?;
        let channel = RetainedValue::with_budget(
            Mutex::new(Channel {
                buffer: [0; CHUNK],
                phase: Phase::Idle,
            }),
            memory,
        )?;
        let reader = ImageRead {
            channel: channel.clone(),
            memory: memory.clone(),
            cancellation: cancellation.clone(),
        };
        let future: Pin<Box<dyn Future<Output = Result<T, E>> + Send>> = Box::pin(create(reader));
        Ok(Self {
            state: Some(State {
                future: Budgeted::new(future, header),
                channel,
                cancellation: cancellation.clone(),
            }),
        })
    }

    /// Execute within this call's byte and poll window. Errors, cancellation, invalid limits and unwinding retire the original continuation before its allocation allowance. A completed or retired input cannot resume.
    pub fn advance(
        &mut self,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> Result<Option<T>, E> {
        let mut state = self.state.take().ok_or_else(|| E::from(invalid()))?;
        if window.bytes == 0 || window.steps == 0 {
            return Err(invalid().into());
        }
        let mut bytes = window.bytes;
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..window.steps {
            state.cancellation.check().map_err(ImageIoError::from)?;
            if let Poll::Ready(value) = Pin::new(&mut state.future).poll(&mut context) {
                return value.map(Some);
            }
            if !state.fill(reader, &mut bytes)? {
                break;
            }
        }
        self.state = Some(state);
        Ok(None)
    }

    /// Drive the same codec synchronously. This does not introduce a second parser or relax input ownership.
    pub fn read(mut self, reader: &mut dyn Read) -> Result<T, E> {
        loop {
            if let Some(value) = self.advance(
                reader,
                ImageDecodeWindow {
                    bytes: 16384,
                    steps: 64,
                },
            )? {
                return Ok(value);
            }
        }
    }
}

impl<T, E> State<T, E> {
    fn fill(&mut self, reader: &mut dyn Read, bytes: &mut usize) -> ImageIoResult<bool> {
        fill(&self.channel, reader, bytes)
    }
}

/// Drive the same codec on the invoking stack. Synchronous callers need neither an allocated continuation nor a shared read buffer; scalar decoding therefore preserves zero-allocation input. Native payloads remain admitted by their owning codec.
pub fn decode_image<T, E, F>(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    create: F,
) -> Result<T, E>
where
    E: From<ImageIoError>,
    F: for<'a> AsyncFnOnce(AsyncImageInput<'a>) -> Result<T, E>,
{
    let channel = Mutex::new(Channel {
        buffer: [0; CHUNK],
        phase: Phase::Idle,
    });
    let input = AsyncImageInput {
        memory,
        cancellation,
        channel: &channel,
    };
    let mut future = std::pin::pin!(create(input));
    let mut context = Context::from_waker(Waker::noop());
    loop {
        cancellation.check().map_err(ImageIoError::from)?;
        if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
            return result;
        }
        let mut bytes = CHUNK;
        fill(&channel, reader, &mut bytes)?;
    }
}

fn fill(channel: &Mutex<Channel>, reader: &mut dyn Read, bytes: &mut usize) -> ImageIoResult<bool> {
    let mut channel = channel.lock();
    match channel.phase {
        Phase::Read(maximum) => {
            if *bytes == 0 {
                return Ok(false);
            }
            let maximum = maximum.min(*bytes).min(CHUNK);
            match reader.read(&mut channel.buffer[..maximum]) {
                Ok(count) if count <= maximum => {
                    *bytes -= count;
                    channel.phase = Phase::Bytes(count);
                }
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error.into()),
            }
        }
        Phase::Checkpoint => channel.phase = Phase::Continue,
        _ => return Err(invalid()),
    }
    Ok(true)
}
