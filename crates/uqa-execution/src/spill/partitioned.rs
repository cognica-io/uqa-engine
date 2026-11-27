//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact arbitrary-byte keys and insertion-ordered row positions behind a growing disk hash directory.

mod directory;
#[cfg(test)]
mod tests;

use crate::{ExecError, ExecResult};
use directory::{Directory, Slot};
use std::io::{self, BufReader, IoSlice, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::{Builder, TempDir};
use uqa_storage::temporary_file::{BlockTemporaryFile, TemporaryFile};

const ROW_BYTES: u64 = 16;
type PositionFile = BlockTemporaryFile<256>;

#[derive(Debug, thiserror::Error)]
enum IndexError {
    #[error(transparent)]
    IO(#[from] io::Error),
    #[error(transparent)]
    Cancelled(#[from] uqa_core::QueryCancelled),
}

type IndexResult<T> = Result<T, IndexError>;

pub(crate) struct PartitionedKeys {
    directory: Directory,
    keys: TemporaryFile,
    rows: Option<PositionFile>,
    location: TempDir,
    owner: &'static str,
}

impl PartitionedKeys {
    pub(crate) fn new(parent: Option<&Path>, owner: &'static str) -> ExecResult<Self> {
        let mut builder = Builder::new();
        builder.prefix("uqa-hash-");
        let location = parent
            .map_or_else(|| builder.tempdir(), |parent| builder.tempdir_in(parent))
            .map_err(|error| {
                ExecError::Other(format!("failed to create {owner} spill directory: {error}"))
            })?;
        let directory =
            Directory::new(location.path(), 64).map_err(|error| spill_error(owner, error))?;
        let keys =
            TemporaryFile::new_in(location.path()).map_err(|error| spill_error(owner, error))?;
        Ok(Self {
            directory,
            keys,
            rows: None,
            location,
            owner,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.location.path()
    }

    fn locate(&mut self, key: &[u8], hash: u64) -> IndexResult<(u64, Slot)> {
        self.directory
            .find(hash, |slot| key_matches(&mut self.keys, slot.key - 1, key))
    }

    pub(crate) fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        self.locate(key, stable_hash(key))
            .map(|(_, slot)| slot.key != 0)
            .map_err(|error| spill_error(self.owner, error))
    }

    pub(crate) fn insert(&mut self, key: &[u8], row: Option<u64>) -> ExecResult<bool> {
        self.insert_hashed(key, stable_hash(key), row)
            .map_err(|error| spill_error(self.owner, error))
    }

    fn insert_hashed(&mut self, key: &[u8], hash: u64, row: Option<u64>) -> IndexResult<bool> {
        let (mut index, mut slot) = self.locate(key, hash)?;
        let inserted = slot.key == 0;
        if !inserted && row.is_none() {
            return Ok(false);
        }
        if inserted && self.directory.needs_growth() {
            self.directory.grow(self.location.path())?;
            index = self.directory.find(hash, |_| Ok(false))?.0;
        }
        if row.is_some() && self.rows.is_none() {
            self.rows = Some(PositionFile::new_in(self.path())?);
        }
        let key_end = self.keys.metadata()?.len();
        let row_end = self
            .rows
            .as_ref()
            .map(|file| file.metadata().map(|meta| meta.len()))
            .transpose()?
            .unwrap_or(0);
        if row_end % ROW_BYTES != 0 {
            return Err(invalid("truncated spill row positions").into());
        }
        let result = (|| {
            if inserted {
                slot.hash = hash;
                slot.key = key_end
                    .checked_add(1)
                    .ok_or_else(|| invalid("spill key offset overflow"))?;
                let length =
                    u64::try_from(key.len()).map_err(|_| invalid("spill key length overflow"))?;
                self.keys.seek(SeekFrom::Start(key_end))?;
                self.keys.write_all_vectored(&mut [
                    IoSlice::new(&length.to_le_bytes()),
                    IoSlice::new(key),
                ])?;
            }
            if let (Some(row), Some(file)) = (row, &mut self.rows) {
                let number = row_end / ROW_BYTES + 1;
                file.seek(SeekFrom::Start(row_end))?;
                file.write_all_vectored(&mut [
                    IoSlice::new(&row.to_le_bytes()),
                    IoSlice::new(&[0; 8]),
                ])?;
                if slot.last == 0 {
                    slot.first = number;
                } else {
                    file.seek(SeekFrom::Start(row_offset(slot.last)? + 8))?;
                    file.write_all(&number.to_le_bytes())?;
                }
                slot.last = number;
            }
            self.directory.publish(index, slot, inserted)
        })();
        if let Err(error) = result {
            // Readers stop at the last position in their published slot. A link past that tail remains unreachable until a successful append replaces it.
            let restore = self.keys.set_len(key_end).and_then(|()| {
                self.rows
                    .as_ref()
                    .map_or(Ok(()), |file| file.set_len(row_end))
            });
            return Err(match restore {
                Ok(()) => error,
                Err(rollback) => {
                    io::Error::other(format!("{error}; spill rollback failed: {rollback}"))
                }
            }
            .into());
        }
        Ok(inserted)
    }

    pub(crate) fn positions(&mut self, key: &[u8]) -> ExecResult<Option<DiskPositions>> {
        self.positions_hashed(key, stable_hash(key))
            .map_err(|error| spill_error(self.owner, error))
    }

    fn positions_hashed(&mut self, key: &[u8], hash: u64) -> IndexResult<Option<DiskPositions>> {
        let (_, slot) = self.locate(key, hash)?;
        if slot.first == 0 {
            return Ok(None);
        }
        let file = self
            .rows
            .as_ref()
            .ok_or_else(|| invalid("missing spill row positions"))?
            .reopen()?;
        let end = file.metadata()?.len();
        if slot.first > slot.last
            || row_offset(slot.last)?
                .checked_add(ROW_BYTES)
                .is_none_or(|offset| offset > end)
        {
            return Err(invalid("invalid spill row bounds").into());
        }
        Ok(Some(DiskPositions {
            input: BufReader::with_capacity(8192, file),
            position: 0,
            next: slot.first,
            last: slot.last,
        }))
    }
}

fn row_offset(number: u64) -> io::Result<u64> {
    number
        .checked_sub(1)
        .and_then(|number| number.checked_mul(ROW_BYTES))
        .ok_or_else(|| invalid("invalid spill row position"))
}

pub(crate) struct DiskPositions<R: Read + Seek = PositionFile> {
    input: BufReader<R>,
    position: u64,
    next: u64,
    last: u64,
}

impl<R: Read + Seek> DiskPositions<R> {
    pub(crate) fn next(&mut self) -> ExecResult<Option<u64>> {
        self.read().map_err(|error| spill_error("join", error))
    }

    fn read(&mut self) -> io::Result<Option<u64>> {
        let number = self.next;
        if number == 0 {
            return Ok(None);
        }
        let offset = row_offset(number)?;
        if offset != self.position {
            let distance = offset
                .checked_sub(self.position)
                .ok_or_else(|| invalid("spill position moved backwards"))?;
            if let Ok(distance) = i64::try_from(distance) {
                self.input.seek_relative(distance)?;
            } else {
                self.input.seek(SeekFrom::Start(offset))?;
            }
        }
        let mut record = [0; 16];
        self.input.read_exact(&mut record)?;
        self.position = offset + ROW_BYTES;
        let (row, link) = record.split_at(8);
        self.next = if number == self.last {
            0
        } else {
            let next = u64::from_le_bytes(link.try_into().unwrap());
            if next <= number || next > self.last {
                return Err(invalid("invalid spill row link"));
            }
            next
        };
        Ok(Some(u64::from_le_bytes(row.try_into().unwrap())))
    }
}

/// Validate the complete declared extent before testing the length or the first differing byte.
fn key_matches(file: &mut TemporaryFile, offset: u64, expected: &[u8]) -> IndexResult<bool> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            invalid("truncated spill key length")
        } else {
            error
        }
    })?;
    let length = u64::from_le_bytes(bytes);
    let end = file.metadata()?.len();
    if offset
        .checked_add(8)
        .and_then(|start| start.checked_add(length))
        .is_none_or(|stop| stop > end)
    {
        return Err(invalid("spill key length exceeds remaining record bytes").into());
    }
    let mut equal = length == expected.len() as u64;
    let mut remaining = length;
    let mut compared = 0;
    let mut scratch = [0; 8192];
    while remaining > 0 {
        uqa_sql::expr::check_statement_cancellation()?;
        let count = usize::try_from(remaining.min(scratch.len() as u64)).unwrap();
        let block = &mut scratch[..count];
        file.read_exact(block)?;
        if equal {
            equal = block == &expected[compared..compared + count];
            compared += count;
        }
        remaining -= count as u64;
    }
    Ok(equal)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn spill_error(owner: &str, error: impl Into<IndexError>) -> ExecError {
    match error.into() {
        IndexError::IO(error) => ExecError::Other(format!("{owner} spill: {error}")),
        IndexError::Cancelled(cancelled) => uqa_sql::SQLError::from(cancelled).into(),
    }
}

pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
