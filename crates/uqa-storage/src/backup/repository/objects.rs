//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;

use super::super::{
    read_frame, BackupError, BackupResult, FrameContext, FrameKind, ImageDigest, ObjectSpan,
};
use super::directory;
use super::LocalRepository;

pub(super) struct ObjectWriter {
    pub file: File,
    hash: Sha256,
    bytes: u64,
}

impl ObjectWriter {
    pub fn new(file: File) -> Self {
        Self {
            file,
            hash: Sha256::new(),
            bytes: 0,
        }
    }
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn digest(&self) -> ImageDigest {
        ImageDigest::from_bytes(self.hash.clone().finalize().into())
    }
}

impl Write for ObjectWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let written = self.file.write(input)?;
        self.hash.update(&input[..written]);
        self.bytes = self
            .bytes
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("backup object size overflow"))?;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(super) struct SpanReader<'a> {
    file: File,
    repository: &'a LocalRepository,
    span: ObjectSpan,
    kind: FrameKind,
    control: StorageReadControl,
    frames: u64,
    offset: u64,
    decoded: BudgetedVec<u8>,
    position: usize,
    failed: bool,
}

impl LocalRepository {
    pub(super) fn span_reader(
        &self,
        span: ObjectSpan,
        kind: FrameKind,
        verify_object: bool,
        control: &StorageReadControl,
    ) -> BackupResult<SpanReader<'_>> {
        control.cancellation().check()?;
        let name = directory::name(span.object().as_bytes());
        let mut file = directory::open_file(&self.objects, directory::text(&name), false)?;
        if file.metadata()?.len() != span.object_bytes() {
            return Err(BackupError::Integrity);
        }
        if verify_object {
            let mut hash = Sha256::new();
            let mut buffer = [0; 16 * 1024];
            let mut remaining = span.object_bytes();
            while remaining != 0 {
                control.cancellation().check()?;
                let count = remaining.min(buffer.len() as u64) as usize;
                let read = match file.read(&mut buffer[..count]) {
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                if read == 0 {
                    return Err(BackupError::Integrity);
                }
                hash.update(&buffer[..read]);
                remaining -= read as u64;
            }
            if ImageDigest::from_bytes(hash.finalize().into()) != span.digest() {
                return Err(BackupError::Integrity);
            }
            super::super::frame::require_end(&mut file, control)?;
        }
        file.seek(SeekFrom::Start(span.offset()))?;
        Ok(SpanReader {
            file,
            repository: self,
            span,
            kind,
            control: control.clone(),
            frames: 0,
            offset: span.offset(),
            decoded: BudgetedVec::new(control.memory()),
            position: 0,
            failed: false,
        })
    }
}

impl SpanReader<'_> {
    fn read_inner(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        self.control.cancellation().check()?;
        while self.position == self.decoded.len() {
            if self.frames == self.span.frames() {
                if self.offset != self.span.offset() + self.span.encoded_bytes() {
                    return Err(BackupError::Invalid("object span frame length"));
                }
                return Ok(0);
            }
            self.decoded = BudgetedVec::new(self.control.memory());
            let remaining = self.span.offset() + self.span.encoded_bytes() - self.offset;
            let mut bounded = (&mut self.file).take(remaining);
            self.decoded = read_frame(
                &mut bounded,
                FrameContext {
                    repository: self.repository.id,
                    object: self.span.object(),
                    offset: self.offset,
                    kind: self.kind,
                },
                &self.repository.protection,
                self.repository.limits,
                &self.control,
            )?;
            self.offset += remaining - bounded.limit();
            self.frames += 1;
            self.position = 0;
        }
        let count = output.len().min(self.decoded.len() - self.position);
        output[..count].copy_from_slice(&self.decoded[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl Read for SpanReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other(BackupError::Finished));
        }
        match self.read_inner(output) {
            Ok(read) => Ok(read),
            Err(error) => {
                self.failed = true;
                Err(io::Error::other(error))
            }
        }
    }
}
