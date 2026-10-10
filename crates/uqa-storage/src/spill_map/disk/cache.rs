//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Only complete immutable logical blocks enter the charged read cache.

use super::{io, BlockTemporaryFile, StorageBackendResult, BLOCK_BYTES};
use std::io::{Read, Seek, SeekFrom};
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation};

#[derive(Clone)]
struct Block {
    offset: Option<u64>,
    bytes: [u8; BLOCK_BYTES],
}

pub(super) struct Blocks {
    entries: Vec<Block>,
    next: usize,
    _memory: MemoryReservation,
}

impl Blocks {
    pub(super) fn new(memory: &MemoryBudget) -> StorageBackendResult<Self> {
        let count = (memory.limit() / 64 / size_of::<Block>()).min(64);
        let (count, reservation) = match memory.reserve(count * size_of::<Block>()) {
            Ok(reservation) => (count, reservation),
            Err(MemoryError::Limit { .. }) => (0, memory.empty_reservation()),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            entries: vec![
                Block {
                    offset: None,
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
            if !self.entries.is_empty() && length.saturating_sub(start) >= BLOCK_BYTES as u64 {
                let slot = if let Some(slot) = self
                    .entries
                    .iter()
                    .position(|entry| entry.offset == Some(start))
                {
                    slot
                } else {
                    let slot = self.next;
                    self.next = (slot + 1) % self.entries.len();
                    let entry = &mut self.entries[slot];
                    // An incomplete read cannot publish a cache entry.
                    entry.offset = None;
                    file.seek(SeekFrom::Start(start)).map_err(io)?;
                    file.read_exact(&mut entry.bytes).map_err(io)?;
                    entry.offset = Some(start);
                    slot
                };
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
