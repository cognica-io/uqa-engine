//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory-to-disk transition and exact bucketed spill storage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tempfile::{Builder as TempBuilder, TempDir};

use crate::{ExecError, ExecResult};

pub(super) const DISK_BUCKETS: u64 = 64;
const COPY_BUFFER_BYTES: usize = 8 * 1024;

pub(crate) struct SeenKeySet {
    memory: BTreeSet<Vec<u8>>,
    memory_bytes: usize,
    budget_bytes: usize,
    spill_directory: Option<PathBuf>,
    pub(super) disk: Option<DiskKeySet>,
}

impl SeenKeySet {
    pub(crate) fn new(budget_bytes: usize, spill_directory: Option<PathBuf>) -> Self {
        Self {
            memory: BTreeSet::new(),
            memory_bytes: 0,
            budget_bytes,
            spill_directory,
            disk: None,
        }
    }

    pub(crate) fn insert(&mut self, key: Vec<u8>) -> ExecResult<bool> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key);
        }
        if self.memory.contains(&key) {
            return Ok(false);
        }

        let fits = self
            .memory_bytes
            .checked_add(key.len())
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory_bytes += key.len();
            self.memory.insert(key);
            return Ok(true);
        }

        // Build the disk set off to the side. A create/migration/write error
        // leaves the original in-memory set intact and is returned to the
        // execution pipeline; no key is silently forgotten.
        let mut disk = DiskKeySet::new(self.spill_directory.as_deref())?;
        for existing in &self.memory {
            if !disk.insert(existing)? {
                return Err(distinct_error(
                    "duplicate key found while migrating DISTINCT state",
                ));
            }
        }
        let inserted = disk.insert(&key)?;
        self.memory.clear();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(inserted)
    }

    pub(crate) fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        match self.disk.as_mut() {
            Some(disk) => disk.contains(key),
            None => Ok(self.memory.contains(key)),
        }
    }

    pub(super) fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    pub(super) fn in_memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    pub(super) fn spill_path(&self) -> Option<&Path> {
        self.disk.as_ref().map(|disk| disk.directory.path())
    }
}

/// Temporary bucketed exact set. Each record is `[u64 length][key bytes]`.
/// Bucket selection is only an accelerator: probes stream and compare the
/// complete record, making equality collision-free even if every hash collides.
pub(super) struct DiskKeySet {
    directory: TempDir,
    pub(super) buckets: BTreeMap<u8, File>,
}

impl DiskKeySet {
    fn new(parent: Option<&Path>) -> ExecResult<Self> {
        let mut builder = TempBuilder::new();
        builder.prefix("uqa-distinct-");
        let directory = parent
            .map_or_else(|| builder.tempdir(), |parent| builder.tempdir_in(parent))
            .map_err(|error| {
                distinct_error(format!(
                    "failed to create DISTINCT spill directory: {error}"
                ))
            })?;
        Ok(Self {
            directory,
            buckets: BTreeMap::new(),
        })
    }

    fn insert(&mut self, key: &[u8]) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % DISK_BUCKETS)
            .map_err(|_| distinct_error("DISTINCT spill bucket exceeds u8"))?;
        if !self.buckets.contains_key(&bucket) {
            let path = self.directory.path().join(format!("bucket-{bucket:02x}"));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| {
                    distinct_error(format!(
                        "failed to create DISTINCT spill bucket {}: {error}",
                        path.display()
                    ))
                })?;
            self.buckets.insert(bucket, file);
        }
        let file = self
            .buckets
            .get_mut(&bucket)
            .ok_or_else(|| distinct_error("DISTINCT spill bucket registration failed"))?;

        file.seek(SeekFrom::Start(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        while let Some(record_len) = read_record_len(file)? {
            let matches = compare_record(file, record_len, key)?;
            if matches {
                return Ok(false);
            }
        }

        let original_len = file.seek(SeekFrom::End(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        let key_len = u64::try_from(key.len())
            .map_err(|_| distinct_error("DISTINCT key length exceeds the on-disk format"))?;
        let write_result = (|| {
            file.write_all(&key_len.to_le_bytes()).map_err(|error| {
                distinct_error(format!("failed to write DISTINCT key length: {error}"))
            })?;
            file.write_all(key).map_err(|error| {
                distinct_error(format!("failed to write DISTINCT key: {error}"))
            })?;
            file.flush().map_err(|error| {
                distinct_error(format!("failed to flush DISTINCT spill bucket: {error}"))
            })
        })();
        if let Err(error) = write_result {
            if let Err(rollback_error) = file.set_len(original_len) {
                return Err(distinct_error(format!(
                    "{error}; failed to roll back partial DISTINCT key: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(true)
    }

    fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % DISK_BUCKETS)
            .map_err(|_| distinct_error("DISTINCT spill bucket exceeds u8"))?;
        let Some(file) = self.buckets.get_mut(&bucket) else {
            return Ok(false);
        };
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        while let Some(record_len) = read_record_len(file)? {
            if compare_record(file, record_len, key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn read_record_len(file: &mut File) -> ExecResult<Option<u64>> {
    let mut encoded = [0_u8; 8];
    match file.read(&mut encoded[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(count) => {
            return Err(distinct_error(format!(
                "invalid DISTINCT spill read count: requested 1 byte, received {count}"
            )));
        }
        Err(error) => {
            return Err(distinct_error(format!(
                "failed to read DISTINCT spill bucket: {error}"
            )));
        }
    }
    file.read_exact(&mut encoded[1..]).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            distinct_error("truncated DISTINCT spill key length")
        } else {
            distinct_error(format!("failed to read DISTINCT key length: {error}"))
        }
    })?;
    Ok(Some(u64::from_le_bytes(encoded)))
}

/// Compare one disk record without allocating a second key-sized buffer.
fn compare_record(file: &mut File, record_len: u64, key: &[u8]) -> ExecResult<bool> {
    let key_len = u64::try_from(key.len())
        .map_err(|_| distinct_error("DISTINCT key length exceeds the on-disk format"))?;
    let mut remaining = record_len;
    let mut offset = 0_usize;
    let mut matches = record_len == key_len;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let copy_buffer_bytes = u64::try_from(COPY_BUFFER_BYTES)
            .map_err(|_| distinct_error("DISTINCT copy buffer exceeds the on-disk length range"))?;
        let take = usize::try_from(remaining.min(copy_buffer_bytes)).map_err(|_| {
            distinct_error("DISTINCT spill key chunk exceeds the addressable memory range")
        })?;
        file.read_exact(&mut buffer[..take]).map_err(|error| {
            if error.kind() == ErrorKind::UnexpectedEof {
                distinct_error("truncated DISTINCT spill key")
            } else {
                distinct_error(format!("failed to read DISTINCT spill key: {error}"))
            }
        })?;
        if matches && buffer[..take] != key[offset..offset + take] {
            matches = false;
        }
        if matches {
            offset += take;
        }
        let consumed = u64::try_from(take)
            .map_err(|_| distinct_error("DISTINCT spill key chunk exceeds the length range"))?;
        remaining -= consumed;
    }
    Ok(matches)
}

pub(super) fn stable_hash(bytes: &[u8]) -> u64 {
    // FNV-1a is deliberately fixed rather than using RandomState: spill files
    // are ephemeral, and equality always verifies the complete key anyway.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn distinct_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}
