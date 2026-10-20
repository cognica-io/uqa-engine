//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequential run descriptors bound metadata independently of the number of sorted runs.

use super::SortedRun;
use crate::spill::{SpillBuffer, SpillFileArena, SpillSegmentDescriptor};
use crate::{ExecError, ExecResult, RowSchema};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use uqa_storage::temporary_file::TemporaryFile;

const DESCRIPTOR_BYTES: usize = 6 * 8;
const CATALOG_BUFFER_BYTES: usize = 4096;

pub(super) struct RunList {
    arena: SpillFileArena,
    schema: RowSchema,
    directory: Option<PathBuf>,
    first: Option<SortedRun>,
    metadata: Option<BufWriter<TemporaryFile>>,
    len: usize,
}

impl RunList {
    pub(super) fn new(
        arena: SpillFileArena,
        schema: RowSchema,
        directory: Option<PathBuf>,
    ) -> Self {
        Self {
            arena,
            schema,
            directory,
            first: None,
            metadata: None,
            len: 0,
        }
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) fn push(&mut self, run: SortedRun) -> ExecResult<()> {
        if self.len == 0 {
            self.first = Some(run);
        } else {
            if self.metadata.is_none() {
                let file = self
                    .directory
                    .as_ref()
                    .map_or_else(TemporaryFile::new, TemporaryFile::new_in)
                    .map_err(io_error)?;
                self.metadata = Some(BufWriter::with_capacity(CATALOG_BUFFER_BYTES, file));
                let first = self.first.take().expect("one retained initial run");
                self.append(first)?;
            }
            self.append(run)?;
        }
        self.len = self
            .len
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("external sort run count overflow".into()))?;
        Ok(())
    }

    fn append(&mut self, run: SortedRun) -> ExecResult<()> {
        let words = run.buffer.into_segment_descriptor()?.0;
        let mut bytes = [0; DESCRIPTOR_BYTES];
        for (chunk, word) in bytes.chunks_exact_mut(8).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        self.metadata
            .as_mut()
            .expect("run catalog writer")
            .write_all(&bytes)
            .map_err(io_error)
    }

    pub(super) fn into_reader(self) -> ExecResult<RunReader> {
        let metadata = self
            .metadata
            .map(|writer| {
                let mut file = writer
                    .into_inner()
                    .map_err(|error| io_error(error.into_error()))?;
                let expected = u64::try_from(self.len)
                    .ok()
                    .and_then(|len| len.checked_mul(DESCRIPTOR_BYTES as u64))
                    .ok_or_else(|| {
                        ExecError::Other("external sort run catalog length overflow".into())
                    })?;
                if file.metadata().map_err(io_error)?.len() != expected {
                    return Err(ExecError::Other(
                        "external sort run catalog length mismatch".into(),
                    ));
                }
                file.seek(SeekFrom::Start(0)).map_err(io_error)?;
                Ok(BufReader::with_capacity(CATALOG_BUFFER_BYTES, file))
            })
            .transpose()?;
        Ok(RunReader {
            arena: self.arena,
            schema: self.schema,
            first: self.first,
            metadata,
            remaining: self.len,
        })
    }
}

pub(super) struct RunReader {
    arena: SpillFileArena,
    schema: RowSchema,
    first: Option<SortedRun>,
    metadata: Option<BufReader<TemporaryFile>>,
    remaining: usize,
}

impl RunReader {
    pub(super) fn next(&mut self) -> ExecResult<Option<SortedRun>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let run = if let Some(first) = self.first.take() {
            first
        } else {
            let mut bytes = [0; DESCRIPTOR_BYTES];
            self.metadata
                .as_mut()
                .expect("multiple runs have a catalog")
                .read_exact(&mut bytes)
                .map_err(io_error)?;
            let mut words = [0; 6];
            for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
                *word = u64::from_le_bytes(chunk.try_into().expect("fixed descriptor word"));
            }
            SortedRun {
                buffer: SpillBuffer::from_segment_descriptor(
                    &self.arena,
                    &self.schema,
                    SpillSegmentDescriptor(words),
                )?,
            }
        };
        self.remaining -= 1;
        Ok(Some(run))
    }
}

fn io_error(error: std::io::Error) -> ExecError {
    ExecError::Other(format!("external sort run catalog: {error}"))
}

#[cfg(test)]
mod tests;
