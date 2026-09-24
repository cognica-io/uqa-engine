//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical encoded index, disk buckets, match flags, and spill transition.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;
use uqa_storage::temporary_file::TemporaryFile as File;

use tempfile::{Builder as TempBuilder, TempDir};
use uqa_storage::temporary_file::TemporaryFile as NamedTempFile;

use crate::distinct::EncodedKey;
use crate::{ExecError, ExecResult};

use super::join_io_error;

pub(super) const HASH_BUCKETS: u64 = 64;

/// One byte per build-side row, held in a temporary file rather than a cardinality-sized `Vec<bool>`. Random updates are required for RIGHT/FULL joins and remain constant-memory.
pub(super) struct MatchFlags {
    file: NamedTempFile,
    rows: u64,
}

impl MatchFlags {
    pub(super) fn new(rows: u64) -> ExecResult<Self> {
        let file =
            NamedTempFile::new().map_err(|error| join_io_error("create match flags", error))?;
        file.as_file()
            .set_len(rows)
            .map_err(|error| join_io_error("size match flags", error))?;
        Ok(Self { file, rows })
    }

    pub(super) fn mark(&mut self, index: u64) -> ExecResult<()> {
        self.check_index(index)?;
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(index))
            .map_err(|error| join_io_error("seek match flags", error))?;
        self.file
            .as_file_mut()
            .write_all(&[1])
            .map_err(|error| join_io_error("write match flag", error))
    }

    pub(super) fn is_marked(&mut self, index: u64) -> ExecResult<bool> {
        self.check_index(index)?;
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(index))
            .map_err(|error| join_io_error("seek match flags", error))?;
        let mut flag = [0_u8; 1];
        self.file
            .as_file_mut()
            .read_exact(&mut flag)
            .map_err(|error| join_io_error("read match flag", error))?;
        Ok(flag[0] != 0)
    }

    pub(super) fn check_index(&self, index: u64) -> ExecResult<()> {
        if index >= self.rows {
            return Err(ExecError::Other(format!(
                "join match flag {index} is outside 0..{}",
                self.rows
            )));
        }
        Ok(())
    }
}

/// Exact, work-memory-bounded build-side hash index. It starts in memory and
/// migrates atomically to bucketed temporary files before the encoded key and
/// row-index records would exceed its byte budget. Disk probes always compare
/// the full key, so bucket hash collisions cannot create false join matches.
pub(super) struct HybridHashIndex {
    memory: HashMap<EncodedKey, Vec<u64>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<DiskHashIndex>,
}

#[derive(Clone, Copy)]
pub(super) enum MemoryMatchSummary {
    Absent,
    Single(u64),
    Multiple,
}

impl HybridHashIndex {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            // AHash keeps a per-index random seed while avoiding SipHash's
            // cryptographic-round overhead after the canonical SQL key has
            // already been encoded byte-for-byte.
            memory: HashMap::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    pub(super) fn insert(&mut self, key: EncodedKey, row_index: u64) -> ExecResult<()> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key, row_index);
        }

        let record_bytes = key
            .len()
            .checked_add(16)
            .ok_or_else(|| ExecError::Other("join hash-index size overflow".into()))?;
        let fits = self
            .memory_bytes
            .checked_add(record_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory.entry(key).or_default().push(row_index);
            self.memory_bytes += record_bytes;
            return Ok(());
        }

        let mut disk = DiskHashIndex::new(None)?;
        for (existing_key, indices) in &self.memory {
            for index in indices {
                disk.insert(existing_key, *index)?;
            }
        }
        disk.insert(&key, row_index)?;
        self.memory.clear();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(())
    }

    pub(super) fn for_each_match(
        &mut self,
        key: &[u8],
        visitor: &mut dyn FnMut(u64) -> ExecResult<()>,
    ) -> ExecResult<bool> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.for_each_match(key, visitor);
        }
        let Some(indices) = self.memory.get(key) else {
            return Ok(false);
        };
        for index in indices {
            visitor(*index)?;
        }
        Ok(!indices.is_empty())
    }

    /// Summarize a probe without allocation when the index still resides in
    /// memory. Disk-backed indexes return `None` and use the streaming probe.
    pub(super) fn memory_match_summary(&self, key: &[u8]) -> Option<MemoryMatchSummary> {
        if self.disk.is_some() {
            return None;
        }
        Some(match self.memory.get(key).map(Vec::as_slice) {
            None | Some([]) => MemoryMatchSummary::Absent,
            Some([index]) => MemoryMatchSummary::Single(*index),
            Some(_) => MemoryMatchSummary::Multiple,
        })
    }

    pub(super) fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    pub(super) fn is_memory_unique(&self) -> bool {
        self.disk.is_none() && self.memory.values().all(|indices| indices.len() == 1)
    }
}

/// Bucket records are `[key_len: u64][key bytes][row_index: u64]`.
pub(super) struct DiskHashIndex {
    directory: TempDir,
    pub(super) buckets: BTreeMap<u8, File>,
}

impl DiskHashIndex {
    pub(super) fn new(parent: Option<&Path>) -> ExecResult<Self> {
        let mut builder = TempBuilder::new();
        builder.prefix("uqa-hash-join-");
        let directory = parent
            .map_or_else(|| builder.tempdir(), |parent| builder.tempdir_in(parent))
            .map_err(|error| join_io_error("create hash directory", error))?;
        Ok(Self {
            directory,
            buckets: BTreeMap::new(),
        })
    }

    pub(super) fn insert(&mut self, key: &[u8], row_index: u64) -> ExecResult<()> {
        let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS)
            .map_err(|_| ExecError::Other("join spill bucket exceeds u8".into()))?;
        if !self.buckets.contains_key(&bucket) {
            let file = File::new_in(self.directory.path())
                .map_err(|error| join_io_error("create hash bucket", error))?;
            self.buckets.insert(bucket, file);
        }
        let file = self
            .buckets
            .get_mut(&bucket)
            .ok_or_else(|| ExecError::Other("join hash bucket registration failed".into()))?;
        let original_len = file
            .seek(SeekFrom::End(0))
            .map_err(|error| join_io_error("seek hash bucket", error))?;
        let key_len = u64::try_from(key.len())
            .map_err(|_| ExecError::Other("join hash key is too large".into()))?;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(&key_len.to_le_bytes())?;
            file.write_all(key)?;
            file.write_all(&row_index.to_le_bytes())?;
            file.flush()
        })();
        if let Err(error) = write_result {
            if let Err(rollback) = file.set_len(original_len) {
                return Err(ExecError::Other(format!(
                    "join spill append hash bucket: {error}; rollback failed: {rollback}"
                )));
            }
            return Err(join_io_error("append hash bucket", error));
        }
        Ok(())
    }

    pub(super) fn for_each_match(
        &mut self,
        key: &[u8],
        visitor: &mut dyn FnMut(u64) -> ExecResult<()>,
    ) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS)
            .map_err(|_| ExecError::Other("join spill bucket exceeds u8".into()))?;
        let Some(file) = self.buckets.get_mut(&bucket) else {
            return Ok(false);
        };
        file.seek(SeekFrom::Start(0))
            .map_err(|error| join_io_error("rewind hash bucket", error))?;
        let file_len = file
            .metadata()
            .map_err(|error| join_io_error("inspect hash bucket", error))?
            .len();
        visit_bucket(file, file_len, key, visitor)
    }
}

fn visit_bucket(
    file: &mut impl Read,
    file_len: u64,
    key: &[u8],
    visitor: &mut dyn FnMut(u64) -> ExecResult<()>,
) -> ExecResult<bool> {
    // Keep a single bounded probe buffer, independent of key size and bucket count.
    let mut reader = BufReader::with_capacity(8 * 1024, file);
    let mut position = 0_u64;
    let mut matched = false;
    while let Some(key_len) = read_u64(&mut reader, "read hash key length")? {
        let record_end = position
            .checked_add(16)
            .and_then(|position| position.checked_add(key_len))
            .ok_or_else(|| ExecError::Other("join hash record offset overflow".into()))?;
        if record_end > file_len {
            return Err(ExecError::Other(format!(
                "join hash key length {key_len} exceeds remaining bucket record bytes"
            )));
        }
        let key_matches = compare_hash_key(&mut reader, key_len, key)?;
        let row_index = read_u64(&mut reader, "read hash row index")?
            .ok_or_else(|| ExecError::Other("truncated join hash row index".into()))?;
        if key_matches {
            visitor(row_index)?;
            matched = true;
        }
        position = record_end;
    }
    Ok(matched)
}

fn compare_hash_key(file: &mut impl BufRead, stored_len: u64, expected: &[u8]) -> ExecResult<bool> {
    let mut remaining = stored_len;
    let mut compared = 0;
    let mut matches = stored_len == expected.len() as u64;
    while remaining > 0 {
        let bytes = file
            .fill_buf()
            .map_err(|error| join_io_error("read hash key", error))?;
        if bytes.is_empty() {
            return Err(ExecError::Other("truncated join hash key".into()));
        }
        let take = remaining.min(bytes.len() as u64) as usize;
        if matches {
            matches = bytes[..take] == expected[compared..compared + take];
            compared += take;
        }
        file.consume(take);
        remaining -= take as u64;
    }
    Ok(matches)
}

fn read_u64(file: &mut impl Read, operation: &str) -> ExecResult<Option<u64>> {
    let mut encoded = [0_u8; 8];
    match file.read(&mut encoded[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(count) => {
            return Err(ExecError::Other(format!(
                "invalid join spill read count: requested 1 byte, received {count}"
            )));
        }
        Err(error) => return Err(join_io_error(operation, error)),
    }
    file.read_exact(&mut encoded[1..]).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            ExecError::Other(format!(
                "truncated join spill record while attempting to {operation}"
            ))
        } else {
            join_io_error(operation, error)
        }
    })?;
    Ok(Some(u64::from_le_bytes(encoded)))
}

pub(super) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use std::io::{self, Cursor};

    struct Reads {
        input: Cursor<Vec<u8>>,
        count: usize,
    }

    impl Read for Reads {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.count += 1;
            self.input.read(bytes)
        }
    }

    #[test]
    fn bucket_probe_coalesces_records_and_visits_every_matching_row() {
        let mut data = Vec::new();
        let key = 7_u64.to_le_bytes();
        for index in 0_u64..128 {
            data.extend_from_slice(&8_u64.to_le_bytes());
            data.extend_from_slice(&(index % 8).to_le_bytes());
            data.extend_from_slice(&index.to_le_bytes());
        }
        let length = data.len() as u64;
        let mut source = Reads {
            input: Cursor::new(data),
            count: 0,
        };
        let mut matched = Vec::new();
        assert!(visit_bucket(&mut source, length, &key, &mut |row| {
            matched.push(row);
            Ok(())
        })
        .unwrap());
        assert_eq!(matched, (7..128).step_by(8).collect::<Vec<_>>());
        assert!(
            source.count <= 2,
            "{} physical reads for one small bucket",
            source.count
        );
    }

    #[test]
    fn truncated_nonmatching_key_does_not_skip_bucket_validation() {
        let data = [100_u64.to_le_bytes(), 1_u64.to_le_bytes()].concat();
        assert!(visit_bucket(
            &mut data.as_slice(),
            data.len() as u64,
            b"short",
            &mut |_| Ok(())
        )
        .unwrap_err()
        .to_string()
        .contains("exceeds remaining bucket record bytes"));
    }
}
