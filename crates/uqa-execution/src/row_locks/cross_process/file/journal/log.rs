//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An append-only generation with a logical base independent of its physical length.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::super::{read_exact_at, write_all_at, CHANGE_ENTRY_MAGIC, CHANGE_ENTRY_SIZE};
use super::codec::{decode_change_entry, encode_change_entry};

const MAGIC: &[u8; 8] = b"UQACHG02";
pub(super) const HEADER_BYTES: u64 = 16;
/// Reclamation copies no more records than it discards, and small prefixes are batched.
const RECLAIM_ENTRIES: u64 = 1024;

pub(super) struct Log {
    file: File,
    start: u64,
    pub(super) base: u64,
    pub(super) next: u64,
}

fn io(error: std::io::Error) -> String {
    format!("row-change journal I/O: {error}")
}

impl Log {
    /// The caller holds the journal lock. A legacy generation has no header and starts at zero; a torn final entry never contributes to its next sequence.
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io)?;
        let length = file.metadata().map_err(io)?.len();
        let (base, start) = if length < 8 {
            (0, 0)
        } else {
            let mut magic = [0; 8];
            read_exact_at(&file, &mut magic, 0).map_err(io)?;
            if &magic == MAGIC {
                let mut base = [0; 8];
                read_exact_at(&file, &mut base, 8).map_err(io)?;
                (u64::from_be_bytes(base), HEADER_BYTES)
            } else if magic[..4] == CHANGE_ENTRY_MAGIC.to_be_bytes() {
                (0, 0)
            } else {
                return Err("row-change journal has an unsupported header".into());
            }
        };
        let count = length.saturating_sub(start) / CHANGE_ENTRY_SIZE;
        let next = base
            .checked_add(count)
            .ok_or("row-change journal sequence overflow")?;
        Ok(Self {
            file,
            start,
            base,
            next,
        })
    }

    fn offset(&self, sequence: u64) -> Result<u64, String> {
        sequence
            .checked_sub(self.base)
            .and_then(|index| index.checked_mul(CHANGE_ENTRY_SIZE))
            .and_then(|offset| offset.checked_add(self.start))
            .ok_or_else(|| "row-change journal offset outside the retained generation".into())
    }

    pub(super) fn read(&self, sequence: u64) -> Result<super::super::PublishedRowChange, String> {
        let mut entry = [0; CHANGE_ENTRY_SIZE as usize];
        read_exact_at(&self.file, &mut entry, self.offset(sequence)?).map_err(io)?;
        decode_change_entry(sequence, &entry)
    }

    pub(super) fn append(
        &mut self,
        changes: &[super::super::PublishedRowChange],
    ) -> Result<(), String> {
        let original = self.offset(self.next)?;
        let next = self
            .next
            .checked_add(changes.len() as u64)
            .ok_or("row-change journal sequence overflow")?;
        let publication = (|| {
            for (index, change) in changes.iter().enumerate() {
                let sequence = self.next + index as u64;
                write_all_at(
                    &self.file,
                    &encode_change_entry(sequence, change),
                    self.offset(sequence)?,
                )
                .map_err(io)?;
            }
            self.file.set_len(self.offset(next)?).map_err(io)
        })();
        if let Err(error) = publication {
            return self.file.set_len(original).map_err(io).and(Err(error));
        }
        self.next = next;
        Ok(())
    }

    /// Replace the path only after the retained suffix is complete. Existing readers keep a valid old descriptor; a killed reclaimer leaves either complete generation and one reusable scratch file.
    pub(super) fn reclaim(&mut self, path: &Path, oldest: u64) -> Result<(), String> {
        if oldest < self.base || oldest > self.next {
            return Err("row-change reader is outside the retained journal".into());
        }
        let discarded = oldest - self.base;
        let retained = self.next - oldest;
        if discarded == 0 || (retained > 0 && discarded < RECLAIM_ENTRIES.max(retained)) {
            return Ok(());
        }
        let mut scratch = path.as_os_str().to_owned();
        scratch.push(".reclaim");
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&scratch)
            .map_err(io)?;
        output.write_all(MAGIC).map_err(io)?;
        output.write_all(&oldest.to_be_bytes()).map_err(io)?;
        self.file
            .seek(SeekFrom::Start(self.offset(oldest)?))
            .map_err(io)?;
        let mut input = (&mut self.file).take(retained * CHANGE_ENTRY_SIZE);
        let copied = std::io::copy(&mut input, &mut output).map_err(io)?;
        if copied != retained * CHANGE_ENTRY_SIZE {
            return Err("row-change journal suffix ended during reclamation".into());
        }
        std::fs::rename(&scratch, path).map_err(io)?;
        *self = Self {
            file: output,
            base: oldest,
            next: self.next,
            start: HEADER_BYTES,
        };
        Ok(())
    }
}
