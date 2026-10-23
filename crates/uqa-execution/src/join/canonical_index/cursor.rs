//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resumable hash probes keep one bounded reader and never collect a duplicate-key fanout.

use super::{compare_hash_key, read_u64};
use crate::distinct::EncodedKey;
use crate::{ExecError, ExecResult};
use std::io::{BufReader, Read};
use std::sync::Arc;
use uqa_storage::temporary_file::TemporaryFile;

pub(in crate::join) struct HashMatches {
    source: MatchSource,
}

enum MatchSource {
    Empty,
    Memory {
        indices: Arc<Vec<u64>>,
        position: usize,
    },
    Disk {
        key: EncodedKey,
        bucket: Box<BucketReader<TemporaryFile>>,
    },
}

impl HashMatches {
    pub(in crate::join) fn empty() -> Self {
        Self {
            source: MatchSource::Empty,
        }
    }

    pub(in crate::join) fn memory(indices: Arc<Vec<u64>>) -> Self {
        Self {
            source: MatchSource::Memory {
                indices,
                position: 0,
            },
        }
    }

    pub(in crate::join) fn disk(key: EncodedKey, file: TemporaryFile, length: u64) -> Self {
        Self {
            source: MatchSource::Disk {
                key,
                bucket: Box::new(BucketReader::new(file, length)),
            },
        }
    }

    pub(in crate::join) fn next(&mut self) -> ExecResult<Option<u64>> {
        match &mut self.source {
            MatchSource::Empty => Ok(None),
            MatchSource::Memory { indices, position } => {
                let value = indices.get(*position).copied();
                if value.is_some() {
                    *position += 1;
                }
                Ok(value)
            }
            MatchSource::Disk { key, bucket } => bucket.next(key),
        }
    }
}

pub(super) struct BucketReader<R: Read> {
    reader: BufReader<R>,
    length: u64,
    position: u64,
}

impl<R: Read> BucketReader<R> {
    pub(super) fn new(file: R, length: u64) -> Self {
        Self {
            reader: BufReader::with_capacity(8 * 1024, file),
            length,
            position: 0,
        }
    }

    pub(super) fn next(&mut self, key: &[u8]) -> ExecResult<Option<u64>> {
        while let Some(key_len) = read_u64(&mut self.reader, "read hash key length")? {
            let record_end = self
                .position
                .checked_add(16)
                .and_then(|position| position.checked_add(key_len))
                .ok_or_else(|| ExecError::Other("join hash record offset overflow".into()))?;
            if record_end > self.length {
                return Err(ExecError::Other(format!(
                    "join hash key length {key_len} exceeds remaining bucket record bytes"
                )));
            }
            let matches = compare_hash_key(&mut self.reader, key_len, key)?;
            let row_index = read_u64(&mut self.reader, "read hash row index")?
                .ok_or_else(|| ExecError::Other("truncated join hash row index".into()))?;
            self.position = record_end;
            if matches {
                return Ok(Some(row_index));
            }
        }
        if self.position != self.length {
            return Err(ExecError::Other("truncated join hash bucket".into()));
        }
        Ok(None)
    }
}
