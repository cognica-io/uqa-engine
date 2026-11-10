//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive input retains incomplete fields and validates UTF-8 within fixed fragments.

use super::{invalid, Channel, ImageIoError, ImageIoResult, Phase, CHUNK};
use crate::{
    memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError},
    CancellationToken,
};
use std::{future::poll_fn, task::Poll};

pub struct AsyncImageInput<'a> {
    pub memory: &'a MemoryBudget,
    pub cancellation: &'a CancellationToken,
    pub(super) channel: &'a parking_lot::Mutex<Channel>,
}

impl AsyncImageInput<'_> {
    /// Yield a unit of codec work without reading bytes. Long metadata or post-input validation loops use this boundary rather than retaining an unbounded poll.
    pub async fn checkpoint(&mut self) -> ImageIoResult<()> {
        self.cancellation.check()?;
        poll_fn(|_| {
            let mut channel = self.channel.lock();
            match channel.phase {
                Phase::Idle => channel.phase = Phase::Checkpoint,
                Phase::Checkpoint => (),
                Phase::Continue => {
                    channel.phase = Phase::Idle;
                    return Poll::Ready(Ok(()));
                }
                _ => return Poll::Ready(Err(invalid())),
            }
            Poll::Pending
        })
        .await
    }

    pub async fn read(&mut self, output: &mut [u8]) -> ImageIoResult<usize> {
        if output.is_empty() {
            self.cancellation.check()?;
            return Ok(0);
        }
        self.fragment(output.len(), |bytes| {
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        })
        .await
    }

    async fn fragment<T>(
        &mut self,
        maximum: usize,
        mut consume: impl FnMut(&[u8]) -> ImageIoResult<T>,
    ) -> ImageIoResult<T> {
        self.cancellation.check()?;
        poll_fn(|_| {
            let mut channel = self.channel.lock();
            match channel.phase {
                Phase::Idle => channel.phase = Phase::Read(maximum.min(CHUNK)),
                Phase::Read(_) => (),
                Phase::Bytes(count) => {
                    channel.phase = Phase::Idle;
                    return Poll::Ready(consume(&channel.buffer[..count]));
                }
                _ => return Poll::Ready(Err(invalid())),
            }
            Poll::Pending
        })
        .await
    }

    pub async fn exact(&mut self, mut output: &mut [u8]) -> ImageIoResult<()> {
        while !output.is_empty() {
            let count = self.read(output).await?;
            if count == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            output = &mut output[count..];
        }
        self.cancellation.check().map_err(Into::into)
    }

    pub async fn array<const N: usize>(&mut self) -> ImageIoResult<[u8; N]> {
        let mut bytes = [0; N];
        self.exact(&mut bytes).await?;
        Ok(bytes)
    }

    pub async fn count(&mut self) -> ImageIoResult<usize> {
        usize::try_from(u64::from_le_bytes(self.array().await?))
            .map_err(|_| MemoryError::SizeOverflow.into())
    }

    pub async fn bytes(&mut self) -> ImageIoResult<Budgeted<Vec<u8>>> {
        self.bytes_with_limit(usize::MAX).await
    }

    pub async fn bytes_with_limit(&mut self, maximum: usize) -> ImageIoResult<Budgeted<Vec<u8>>> {
        let length = self.length(maximum).await?;
        let mut bytes = BudgetedVec::new(self.memory);
        bytes.reserve(length)?;
        while bytes.len() < length {
            let count = self
                .fragment(length - bytes.len(), |fragment| {
                    bytes.extend_from_slice(fragment)?;
                    Ok(fragment.len())
                })
                .await?;
            require_progress(count)?;
        }
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(bytes, memory))
    }

    pub async fn text(&mut self) -> ImageIoResult<Budgeted<String>> {
        self.text_with_limit(usize::MAX).await
    }

    /// Copy an already validated native field with one checkpoint per fixed fragment and no source reads. The independent destination keeps its original admission across yields.
    pub async fn copy_text(&mut self, source: &str) -> ImageIoResult<Budgeted<String>> {
        self.cancellation.check()?;
        let mut text = BudgetedString::new(self.memory);
        text.reserve(source.len())?;
        let mut utf8 = Utf8 {
            bytes: [0; 4],
            length: 0,
        };
        for fragment in source.as_bytes().chunks(CHUNK) {
            self.checkpoint().await?;
            utf8.append(fragment, &mut text)?;
        }
        let (text, memory) = text.into_parts();
        Ok(Budgeted::new(text, memory))
    }

    pub async fn text_with_limit(&mut self, maximum: usize) -> ImageIoResult<Budgeted<String>> {
        let length = self.length(maximum).await?;
        let mut text = BudgetedString::new(self.memory);
        text.reserve(length)?;
        let mut remaining = length;
        let mut utf8 = Utf8 {
            bytes: [0; 4],
            length: 0,
        };
        while remaining != 0 {
            let count = self
                .fragment(remaining, |fragment| {
                    utf8.append(fragment, &mut text)?;
                    Ok(fragment.len())
                })
                .await?;
            require_progress(count)?;
            remaining -= count;
        }
        if utf8.length != 0 {
            return Err(invalid());
        }
        let (text, memory) = text.into_parts();
        Ok(Budgeted::new(text, memory))
    }

    async fn length(&mut self, maximum: usize) -> ImageIoResult<usize> {
        let length = self.count().await?;
        if length > maximum {
            return Err(MemoryError::Limit {
                required: length,
                limit: maximum,
            }
            .into());
        }
        Ok(length)
    }

    pub async fn end(&mut self) -> ImageIoResult<()> {
        if self.read(&mut [0]).await? != 0 {
            return Err(invalid());
        }
        self.cancellation.check().map_err(ImageIoError::from)
    }
}

fn require_progress(count: usize) -> ImageIoResult<()> {
    if count == 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    Ok(())
}

struct Utf8 {
    bytes: [u8; 4],
    length: usize,
}

impl Utf8 {
    fn append(&mut self, mut bytes: &[u8], text: &mut BudgetedString) -> ImageIoResult<()> {
        if self.length != 0 {
            let width = match self.bytes[0] {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(invalid()),
            };
            let count = (width - self.length).min(bytes.len());
            self.bytes[self.length..self.length + count].copy_from_slice(&bytes[..count]);
            self.length += count;
            bytes = &bytes[count..];
            if self.length < width {
                if std::str::from_utf8(&self.bytes[..self.length])
                    .is_err_and(|error| error.error_len().is_some())
                {
                    return Err(invalid());
                }
                return Ok(());
            }
            text.push_str(std::str::from_utf8(&self.bytes[..width]).map_err(|_| invalid())?)?;
            self.length = 0;
        }
        match std::str::from_utf8(bytes) {
            Ok(value) => text.push_str(value)?,
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                text.push_str(std::str::from_utf8(&bytes[..valid]).map_err(|_| invalid())?)?;
                self.length = bytes.len() - valid;
                self.bytes[..self.length].copy_from_slice(&bytes[valid..]);
            }
            Err(_) => return Err(invalid()),
        }
        Ok(())
    }
}
