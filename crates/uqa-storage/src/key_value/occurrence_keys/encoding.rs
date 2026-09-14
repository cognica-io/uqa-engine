//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and controlled key construction share one canonical byte encoder.

use super::super::codec::key_segment_length;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{BudgetedVec, MemoryError};

#[derive(Clone, Copy)]
pub(in crate::key_value) enum Part<'a> {
    Byte(u8),
    Segment(&'a [u8]),
    Number(u64),
}

trait Buffer {
    fn reserve(&mut self, count: usize) -> StorageBackendResult<()>;
    fn push(&mut self, byte: u8) -> StorageBackendResult<()>;
}
impl Buffer for Vec<u8> {
    fn reserve(&mut self, count: usize) -> StorageBackendResult<()> {
        self.try_reserve_exact(count).map_err(MemoryError::from)?;
        Ok(())
    }
    fn push(&mut self, byte: u8) -> StorageBackendResult<()> {
        Vec::push(self, byte);
        Ok(())
    }
}
impl Buffer for BudgetedVec<u8> {
    fn reserve(&mut self, count: usize) -> StorageBackendResult<()> {
        BudgetedVec::reserve(self, count)?;
        Ok(())
    }
    fn push(&mut self, byte: u8) -> StorageBackendResult<()> {
        BudgetedVec::push(self, byte)?;
        Ok(())
    }
}

fn encode<'a, B: Buffer>(
    mut output: B,
    parts: impl Iterator<Item = Part<'a>> + Clone,
    mut poll: impl FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<B> {
    poll()?;
    let mut count = 0usize;
    for part in parts.clone() {
        poll()?;
        let length = match part {
            Part::Byte(_) => 1,
            Part::Number(_) => 8,
            Part::Segment(bytes) => {
                key_segment_length(bytes.len())?;
                bytes
                    .len()
                    .checked_add(4)
                    .ok_or(MemoryError::SizeOverflow)?
            }
        };
        count = count.checked_add(length).ok_or(MemoryError::SizeOverflow)?;
    }
    output.reserve(count)?;
    for part in parts {
        poll()?;
        match part {
            Part::Byte(byte) => output.push(byte)?,
            Part::Number(number) => {
                for byte in number.to_be_bytes() {
                    output.push(byte)?;
                }
            }
            Part::Segment(bytes) => {
                for byte in key_segment_length(bytes.len())?.to_be_bytes() {
                    output.push(byte)?;
                }
                for (index, byte) in bytes.iter().copied().enumerate() {
                    if index % 1024 == 0 {
                        poll()?;
                    }
                    output.push(byte)?;
                }
            }
        }
    }
    poll()?;
    Ok(output)
}

pub(in crate::key_value) fn key(
    table: &str,
    tag: u8,
    kind: Option<u8>,
    tail: &[Part<'_>],
) -> StorageBackendResult<Vec<u8>> {
    encode(
        Vec::new(),
        [Part::Byte(tag), Part::Segment(table.as_bytes())]
            .into_iter()
            .chain(kind.map(Part::Byte))
            .chain(tail.iter().copied()),
        || Ok(()),
    )
}

pub(in crate::key_value) fn controlled(
    table: &str,
    tag: u8,
    kind: Option<u8>,
    tail: &[Part<'_>],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    encode(
        BudgetedVec::new(control.memory()),
        [Part::Byte(tag), Part::Segment(table.as_bytes())]
            .into_iter()
            .chain(kind.map(Part::Byte))
            .chain(tail.iter().copied()),
        || control.check(),
    )
}
