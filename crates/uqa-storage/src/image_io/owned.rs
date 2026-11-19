//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A pinned continuation keeps an admitted owner and its borrowed codec together.

use super::{ImagePreparation, ImageRead, ImageReader};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use parking_lot::Mutex;
use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use uqa_core::memory::MemoryReservation;

/// An independently retained immutable image source. The owner carries its payload allowance and original cancellation; opening a reader may only initialize its existing bounded codec state.
pub trait ImageOwner: Send + Sync + 'static {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>>;
}

const WINDOW: usize = 16 * 1024;

enum Command {
    Prepare(usize),
    Read(usize),
}
enum Response {
    Prepared(ImagePreparation),
    Bytes(usize),
}
enum ExchangeState {
    Idle,
    Command(Command),
    Response(Response),
}

struct Exchange {
    state: ExchangeState,
    bytes: [u8; WINDOW],
}

struct Mailbox {
    exchange: Mutex<Exchange>,
    _memory: MemoryReservation,
}

type Continuation = Pin<Box<dyn Future<Output = StorageBackendResult<()>> + Send>>;

struct OwnedRead {
    // Retire the borrowed codec and its owner before their allocation leases.
    continuation: Continuation,
    mailbox: Arc<Mailbox>,
    _memory: MemoryReservation,
}

impl ImageReader<'static> {
    /// Consume an admitted source into a movable reader that outlives the capturing Engine. The source opens lazily on first preparation/read. Its complete lifetime, a fixed transfer window and the compiler-pinned continuation are admitted before allocation. No runtime, task, thread or source replay is involved.
    pub fn from_owner<O: ImageOwner>(
        owner: O,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Mailbox>())?;
        let mailbox = Arc::new(Mailbox {
            exchange: Mutex::new(Exchange {
                state: ExchangeState::Idle,
                bytes: [0; WINDOW],
            }),
            _memory: memory,
        });
        let continuation = serve(owner, control.clone(), Arc::clone(&mailbox));
        let memory = control.memory().reserve(size_of_val(&continuation))?;
        let continuation: Continuation = Box::pin(continuation);
        Self::new(
            OwnedRead {
                continuation,
                mailbox,
                _memory: memory,
            },
            control,
        )
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid owned image continuation".into())
}

impl OwnedRead {
    fn invoke(&mut self, command: Command) -> StorageBackendResult<Response> {
        {
            let mut exchange = self.mailbox.exchange.lock();
            if !matches!(exchange.state, ExchangeState::Idle) {
                return Err(invalid());
            }
            exchange.state = ExchangeState::Command(command);
        }
        // This private continuation yields only after completing the supplied command. Each invocation polls once; the caller remains its only scheduler.
        if let Poll::Ready(result) = self
            .continuation
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            result?;
        }
        let mut exchange = self.mailbox.exchange.lock();
        let ExchangeState::Response(response) =
            std::mem::replace(&mut exchange.state, ExchangeState::Idle)
        else {
            return Err(invalid());
        };
        Ok(response)
    }
}

impl ImageRead for OwnedRead {
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        let Response::Prepared(progress) = self.invoke(Command::Prepare(maximum_positions))? else {
            return Err(invalid());
        };
        Ok(progress)
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let limit = output.len().min(WINDOW);
        let Response::Bytes(count) = self.invoke(Command::Read(limit))? else {
            return Err(invalid());
        };
        if count > limit {
            return Err(invalid());
        }
        output[..count].copy_from_slice(&self.mailbox.exchange.lock().bytes[..count]);
        Ok(count)
    }
}

async fn serve<O: ImageOwner>(
    owner: O,
    control: StorageReadControl,
    mailbox: Arc<Mailbox>,
) -> StorageBackendResult<()> {
    let mut reader = owner.image_reader(&control)?;
    loop {
        let command = poll_fn(|cx| {
            let mut exchange = mailbox.exchange.lock();
            if matches!(exchange.state, ExchangeState::Command(_)) {
                let ExchangeState::Command(command) =
                    std::mem::replace(&mut exchange.state, ExchangeState::Idle)
                else {
                    unreachable!()
                };
                Poll::Ready(command)
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        match command {
            Command::Prepare(maximum) => {
                let progress = reader.prepare(maximum)?;
                mailbox.exchange.lock().state =
                    ExchangeState::Response(Response::Prepared(progress));
            }
            Command::Read(maximum) => {
                let mut exchange = mailbox.exchange.lock();
                let count = reader.read(&mut exchange.bytes[..maximum])?;
                exchange.state = ExchangeState::Response(Response::Bytes(count));
                if count == 0 && maximum != 0 {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
