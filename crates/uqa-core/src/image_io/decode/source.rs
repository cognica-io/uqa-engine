//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested codecs borrow their owner's transport while retaining independent field continuations.

use super::{AsyncImageInput, Channel, Phase, CHUNK};
use crate::{
    image_io::{invalid, ImageIoError, ImageIoResult},
    memory::MemoryBudget,
    CancellationToken,
};
use parking_lot::Mutex;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

/// An owning image transport may transform bytes but must route reads and work checkpoints through its enclosing native input. Implementations must not await an external executor or retain an unbounded computation between checkpoints.
pub trait AsyncImageRead: Send {
    fn read(&mut self, output: &mut [u8]) -> impl Future<Output = ImageIoResult<usize>> + Send;
    fn checkpoint(&mut self) -> impl Future<Output = ImageIoResult<()>> + Send;
}

impl AsyncImageRead for AsyncImageInput<'_> {
    async fn read(&mut self, output: &mut [u8]) -> ImageIoResult<usize> {
        self.read(output).await
    }

    async fn checkpoint(&mut self) -> ImageIoResult<()> {
        self.checkpoint().await
    }
}

/// Run a child grammar through its owner's asynchronous transport. The enclosing admitted future owns both fixed buffers and the child continuation; this introduces no allocation or whole-image copy. Every child poll consumes an outer checkpoint, including retries and work that reads no bytes.
pub async fn decode_image_from<T, E, F, R>(
    reader: &mut R,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    create: F,
) -> Result<T, E>
where
    E: From<ImageIoError>,
    F: for<'a> AsyncFnOnce(AsyncImageInput<'a>) -> Result<T, E>,
    R: AsyncImageRead,
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
    let mut buffer = [0; CHUNK];
    loop {
        cancellation.check().map_err(ImageIoError::from)?;
        reader.checkpoint().await?;
        {
            let mut context = Context::from_waker(Waker::noop());
            if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                return result;
            }
        }
        let maximum = {
            let mut channel = channel.lock();
            match channel.phase {
                Phase::Read(maximum) => maximum.min(CHUNK),
                Phase::Checkpoint => {
                    channel.phase = Phase::Continue;
                    continue;
                }
                _ => return Err(invalid().into()),
            }
        };
        match reader.read(&mut buffer[..maximum]).await {
            Ok(count) if count <= maximum => {
                let mut channel = channel.lock();
                channel.buffer[..count].copy_from_slice(&buffer[..count]);
                channel.phase = Phase::Bytes(count);
            }
            Ok(_) => return Err(invalid().into()),
            Err(ImageIoError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}
