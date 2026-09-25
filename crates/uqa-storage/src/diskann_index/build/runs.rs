//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Charged block buffers prevent tiny membership/edge records from repeatedly rewriting or decrypting one cipher block.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use uqa_core::memory::BudgetedVec;

use super::temporary::{io_error, File, TemporaryRun};
use super::{invalid, DiskANNTemporaryBudget};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const BLOCK: usize = 4096;

pub(super) struct RunWriter {
    run: TemporaryRun,
    buffer: BudgetedVec<u8>,
    control: StorageReadControl,
    failed: bool,
}

impl RunWriter {
    pub(super) fn new(
        directory: &Path,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            run: TemporaryRun::new(directory, temporary, control)?,
            buffer: BudgetedVec::new(control.memory()),
            control: control.clone(),
            failed: false,
        })
    }

    pub(super) fn append(&mut self, mut bytes: &[u8]) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.failed {
            return Err(invalid("buffered run previously failed"));
        }
        self.failed = true;
        if !bytes.is_empty() {
            self.prepare()?;
        }
        while !bytes.is_empty() {
            self.control.check()?;
            let count = bytes.len().min(BLOCK - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..count])?;
            bytes = &bytes[count..];
            if self.buffer.len() == BLOCK {
                self.flush()?;
            }
        }
        self.failed = false;
        Ok(())
    }

    pub(super) fn prepare(&mut self) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.buffer.capacity() == 0 {
            self.buffer.reserve(BLOCK)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> StorageBackendResult<()> {
        if !self.buffer.is_empty() {
            self.run.append(&self.buffer, &self.control)?;
            self.buffer.clear();
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> StorageBackendResult<TemporaryRun> {
        self.control.check()?;
        if self.failed {
            return Err(invalid("buffered run previously failed"));
        }
        self.flush()?;
        Ok(self.run)
    }
}

pub(super) struct RunReader<'a> {
    file: &'a mut File,
    buffer: BudgetedVec<u8>,
    position: usize,
    length: usize,
    block_start: u64,
    control: &'a StorageReadControl,
}

impl<'a> RunReader<'a> {
    pub(super) fn new(
        file: &'a mut File,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::at(file, 0, control)
    }

    pub(super) fn at(
        file: &'a mut File,
        offset: u64,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut buffer = BudgetedVec::new(control.memory());
        buffer.extend_from_slice(&[0; BLOCK])?;
        let mut reader = Self {
            file,
            buffer,
            position: 0,
            length: 0,
            block_start: 0,
            control,
        };
        reader.seek_to(offset)?;
        Ok(reader)
    }

    pub(super) fn seek_to(&mut self, offset: u64) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.length != 0
            && offset >= self.block_start
            && offset - self.block_start <= self.length as u64
        {
            self.position = (offset - self.block_start) as usize;
            return Ok(());
        }
        let prefix = (offset % BLOCK as u64) as usize;
        self.block_start = offset - prefix as u64;
        self.file
            .seek(SeekFrom::Start(self.block_start))
            .map_err(io_error)?;
        self.position = prefix;
        self.length = 0;
        if prefix != 0 {
            self.length = self.file.read(&mut self.buffer).map_err(io_error)?;
            if self.length < prefix {
                return Err(io_error(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "DiskANN run starts beyond its logical end",
                )));
            }
        }
        Ok(())
    }

    pub(super) fn record<const N: usize>(&mut self) -> StorageBackendResult<[u8; N]> {
        self.control.check()?;
        let mut bytes = [0; N];
        let mut copied = 0;
        while copied < N {
            if self.position == self.length {
                self.control.check()?;
                self.block_start += self.length as u64;
                self.length = self.file.read(&mut self.buffer).map_err(io_error)?;
                self.position = 0;
                if self.length == 0 {
                    return Err(io_error(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "incomplete DiskANN temporary record",
                    )));
                }
            }
            let count = (N - copied).min(self.length - self.position);
            bytes[copied..copied + count]
                .copy_from_slice(&self.buffer[self.position..self.position + count]);
            copied += count;
            self.position += count;
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_records_cross_blocks_without_repeated_cipher_work_or_lost_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let control = StorageReadControl::with_limit(BLOCK);
        let mut writer = RunWriter::new(directory.path(), &temporary, &control).unwrap();
        for id in 0_u64..700 {
            let mut record = [0; 10];
            record[..8].copy_from_slice(&id.to_le_bytes());
            record[8] = (id % 256) as u8;
            record[9] = record[8] ^ 0x80;
            writer.append(&record).unwrap();
        }
        let run = writer.finish().unwrap();
        assert_eq!(control.memory().used(), 0);
        assert_eq!(run.block_io_counts(), (0, 7000 + 2 * 43));
        run.read(&control, |file| {
            let mut reader = RunReader::new(file, &control)?;
            for id in 0_u64..700 {
                let record = reader.record::<10>()?;
                assert_eq!(u64::from_le_bytes(record[..8].try_into().unwrap()), id);
                assert_eq!(record[8], (id % 256) as u8);
                assert_eq!(record[9], record[8] ^ 0x80);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(run.block_io_counts(), (2, 7000 + 2 * 43));
        drop(run);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(temporary.used(), 0);
    }
}
