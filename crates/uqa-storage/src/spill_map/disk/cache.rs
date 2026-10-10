//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authenticated immutable prefixes share the charged read cache across appends.

use super::{io, BlockTemporaryFile, StorageBackendResult, BLOCK_BYTES};
use std::io::{Read, Seek, SeekFrom};
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation};

#[derive(Clone)]
struct Block {
    offset: Option<u64>,
    valid: usize,
    bytes: [u8; BLOCK_BYTES],
}

pub(super) struct Blocks {
    entries: Vec<Block>,
    next: usize,
    _memory: MemoryReservation,
}

impl Blocks {
    pub(super) fn workspace_bytes(memory: &MemoryBudget) -> usize {
        (memory.limit() / 64 / size_of::<Block>()).min(64) * size_of::<Block>()
    }

    pub(super) fn new(memory: &MemoryBudget) -> StorageBackendResult<Self> {
        let count = Self::workspace_bytes(memory) / size_of::<Block>();
        let (count, reservation) = match memory.reserve(count * size_of::<Block>()) {
            Ok(reservation) => (count, reservation),
            Err(MemoryError::Limit { .. }) => (0, memory.empty_reservation()),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            entries: vec![
                Block {
                    offset: None,
                    valid: 0,
                    bytes: [0; BLOCK_BYTES]
                };
                count
            ],
            next: 0,
            _memory: reservation,
        })
    }

    pub(super) fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.offset = None;
        }
    }

    pub(super) fn read(
        &mut self,
        file: &mut BlockTemporaryFile<BLOCK_BYTES>,
        mut offset: u64,
        mut output: &mut [u8],
    ) -> StorageBackendResult<()> {
        let length = file.metadata().map_err(io)?.len();
        while !output.is_empty() {
            let start = offset / BLOCK_BYTES as u64 * BLOCK_BYTES as u64;
            let within = (offset - start) as usize;
            let take = output.len().min(BLOCK_BYTES - within);
            let available = length.saturating_sub(start).min(BLOCK_BYTES as u64) as usize;
            if !self.entries.is_empty() && within + take <= available {
                let cached = self
                    .entries
                    .iter()
                    .position(|entry| entry.offset == Some(start));
                let slot = cached.unwrap_or_else(|| {
                    let slot = self.next;
                    self.next = (slot + 1) % self.entries.len();
                    slot
                });
                if cached.is_none() || self.entries[slot].valid < within + take {
                    let entry = &mut self.entries[slot];
                    // Appends preserve earlier bytes. A cached prefix remains
                    // usable until a request needs the newly appended suffix.
                    // Failed appends clear the cache before addresses are reused.
                    entry.offset = None;
                    file.seek(SeekFrom::Start(start)).map_err(io)?;
                    file.read_exact(&mut entry.bytes[..available]).map_err(io)?;
                    entry.valid = available;
                    entry.offset = Some(start);
                }
                output[..take].copy_from_slice(&self.entries[slot].bytes[within..within + take]);
            } else {
                file.seek(SeekFrom::Start(offset)).map_err(io)?;
                file.read_exact(&mut output[..take]).map_err(io)?;
            }
            offset += take as u64;
            output = &mut output[take..];
        }
        Ok(())
    }
}
