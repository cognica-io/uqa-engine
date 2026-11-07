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

/// Whole-object hashing advances through the same bounded owner during capture and restore verification.
pub(super) struct ObjectVerification {
    file: File,
    expected: ImageDigest,
    remaining: u64,
    hash: Sha256,
    bytes: u64,
    complete: bool,
}

impl ObjectVerification {
    pub(super) fn new(file: File, expected: ImageDigest, bytes: u64) -> BackupResult<Self> {
        if file.metadata()?.len() != bytes {
            return Err(BackupError::Integrity);
        }
        Ok(Self {
            file,
            expected,
            remaining: bytes,
            hash: Sha256::new(),
            bytes: 0,
            complete: false,
        })
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> BackupResult<bool> {
        if self.complete {
            return Ok(true);
        }
        control.check()?;
        if maximum == 0 {
            return Err(BackupError::Invalid("zero object verification window"));
        }
        let mut buffer = [0; 16 * 1024];
        let size = self.remaining.min(buffer.len().min(maximum) as u64) as usize;
        let read = match self.file.read(&mut buffer[..size]) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if read == 0 && self.remaining != 0 {
            return Err(BackupError::Integrity);
        }
        self.hash.update(&buffer[..read]);
        self.remaining -= read as u64;
        self.bytes = self
            .bytes
            .checked_add(read as u64)
            .ok_or(BackupError::Invalid("object byte count overflow"))?;
        if self.remaining == 0 {
            super::super::frame::require_end(&mut self.file, control)?;
            if ImageDigest::from_bytes(self.hash.clone().finalize().into()) != self.expected {
                return Err(BackupError::Integrity);
            }
            self.complete = true;
        }
        Ok(self.complete)
    }

    fn into_file(self) -> BackupResult<File> {
        if !self.complete {
            return Err(BackupError::Finished);
        }
        Ok(self.file)
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
            let mut verification =
                ObjectVerification::new(file, span.digest(), span.object_bytes())?;
            while !verification.advance(16 * 1024, control)? {}
            file = verification.into_file()?;
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
            if self.decoded.is_empty() && (self.span.frames() != 1 || self.kind != FrameKind::Unit)
            {
                return Err(BackupError::Invalid("noncanonical empty object frame"));
            }
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
