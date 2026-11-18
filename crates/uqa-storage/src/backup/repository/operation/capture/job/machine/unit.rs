//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current and inherited units share exact bounded read, hash and frame transitions.

use super::{
    add, BackupError, BackupImageSource, BackupOperation, BackupResult, CapturePack, Machine,
    Pending, Reference, Stage, TemporaryFile, UnitMetadata,
};
use crate::backup::{FrameKind, ImageDigest, ObjectSpan, UnitFingerprint};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(super) enum Mode {
    Current,
    Compare {
        digest: ImageDigest,
        span: ObjectSpan,
        bytes: u64,
        file: TemporaryFile,
    },
    Spooled {
        digest: ImageDigest,
        file: TemporaryFile,
    },
    Reused {
        digest: ImageDigest,
        span: ObjectSpan,
    },
}

pub(super) struct Unit {
    pub(super) metadata: UnitMetadata,
    mode: Mode,
    hash: Option<Sha256>,
    expected: Option<u64>,
    read: u64,
    offset: u64,
    frames: u64,
}

impl Unit {
    pub(super) fn new(
        metadata: UnitMetadata,
        expected: Option<u64>,
        source_hash: crate::backup::SourceUnitHash,
    ) -> Self {
        Self {
            metadata,
            mode: Mode::Current,
            hash: matches!(source_hash, crate::backup::SourceUnitHash::Repository)
                .then(Sha256::new),
            expected,
            read: 0,
            offset: 0,
            frames: 0,
        }
    }

    fn complete_digest(&mut self, source: &impl BackupImageSource) -> BackupResult<ImageDigest> {
        match self.hash.take() {
            Some(hash) => Ok(ImageDigest::from_bytes(hash.finalize().into())),
            None => match source.current_hash() {
                crate::backup::SourceUnitHash::Complete(fp)
                    if fp.decoded_bytes == self.metadata.bytes =>
                {
                    Ok(fp.digest)
                }
                _ => Err(BackupError::Integrity),
            },
        }
    }

    fn record(&mut self, read: usize, maximum: usize) -> BackupResult<()> {
        if read > maximum {
            return Err(BackupError::Invalid("source read exceeds output buffer"));
        }
        self.read = self
            .read
            .checked_add(read as u64)
            .ok_or(BackupError::Integrity)?;
        if self
            .expected
            .is_some_and(|bytes| self.read > bytes || (read == 0 && self.read != bytes))
        {
            return Err(BackupError::Integrity);
        }
        if read == 0 {
            self.metadata.bytes = self.read;
            self.expected = Some(self.read);
        }
        Ok(())
    }
}

impl<'a, S: BackupImageSource> Machine<'a, S> {
    pub(super) fn match_parent(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_bytes: usize,
        maximum_positions: usize,
    ) -> BackupResult<()> {
        let unit = self.unit.as_mut().expect("selected unit");
        if let Some(parent) = &mut self.parent {
            if parent
                .current()
                .is_some_and(|entry| entry.address() < unit.metadata.address())
            {
                parent.advance(maximum_bytes)?;
                return Ok(());
            }
            if let Some(entry) = parent.current().filter(|entry| {
                entry.address() == unit.metadata.address()
                    && entry.kind() == unit.metadata.kind
                    && unit
                        .expected
                        .is_none_or(|bytes| entry.decoded_bytes() == bytes)
            }) {
                let fingerprint = UnitFingerprint {
                    digest: entry.digest(),
                    decoded_bytes: entry.decoded_bytes(),
                };
                if self
                    .source
                    .as_mut()
                    .expect("selected source")
                    .reuse_current(fingerprint)?
                {
                    add(&mut self.statistics.certified_reused_units, 1)?;
                    add(
                        &mut self.statistics.certified_reused_bytes,
                        fingerprint.decoded_bytes,
                    )?;
                    unit.metadata.bytes = fingerprint.decoded_bytes;
                    unit.mode = Mode::Reused {
                        digest: fingerprint.digest,
                        span: entry.object(),
                    };
                    self.stage = Stage::UnitComplete;
                    return Ok(());
                }
                if !self
                    .source
                    .as_mut()
                    .expect("selected source")
                    .prepare_current(maximum_positions)?
                {
                    return Ok(());
                }
                unit.mode = Mode::Compare {
                    digest: entry.digest(),
                    span: entry.object(),
                    bytes: entry.decoded_bytes(),
                    file: TemporaryFile::with_file_budget(&self.files)?,
                };
                self.stage = Stage::Compare;
                return Ok(());
            }
        }
        if !self
            .source
            .as_mut()
            .expect("selected source")
            .prepare_current(maximum_positions)?
        {
            return Ok(());
        }
        self.begin_write(operation)
    }

    fn begin_write(&mut self, operation: &mut BackupOperation<'a>) -> BackupResult<()> {
        self.pending.reserve(1)?;
        if self.pack.is_none() {
            self.pack = Some(CapturePack::new(operation, &self.control)?);
            self.retention_limit = (self.control.memory().available() / 4).min(64 * 1024);
        }
        let unit = self.unit.as_mut().expect("selected unit");
        unit.offset = self.pack.as_ref().expect("active output pack").bytes();
        unit.frames = 0;
        unit.read = 0;
        self.filled = 0;
        self.stage = Stage::Write;
        Ok(())
    }

    pub(super) fn compare(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_bytes: usize,
    ) -> BackupResult<()> {
        let unit = self.unit.as_mut().expect("selected comparison");
        let read = self
            .source
            .as_mut()
            .expect("comparison owns source")
            .read_current(&mut self.frame[..maximum_bytes])?;
        unit.record(read, maximum_bytes)?;
        let Mode::Compare { file, .. } = &mut unit.mode else {
            unreachable!("comparison stage owns spool");
        };
        if read != 0 {
            if let Some(hash) = &mut unit.hash {
                hash.update(&self.frame[..read]);
            }
            add(&mut self.statistics.hashed_payload_bytes, read as u64)?;
            file.write_all(&self.frame[..read])?;
            return Ok(());
        }
        let digest = unit.complete_digest(self.source.as_ref().expect("selected source"))?;
        let Mode::Compare {
            digest: expected,
            span,
            bytes,
            mut file,
        } = std::mem::replace(&mut unit.mode, Mode::Current)
        else {
            unreachable!()
        };
        if digest == expected && unit.metadata.bytes == bytes {
            unit.mode = Mode::Reused { digest, span };
            self.stage = Stage::UnitComplete;
            Ok(())
        } else {
            file.seek(SeekFrom::Start(0))?;
            unit.mode = Mode::Spooled { digest, file };
            self.begin_write(operation)
        }
    }

    pub(super) fn write_unit(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_bytes: usize,
    ) -> BackupResult<()> {
        let unit = self.unit.as_mut().expect("writing selected unit");
        let size = maximum_bytes.min(self.frame.len() - self.filled);
        let output = &mut self.frame[self.filled..self.filled + size];
        let current = matches!(unit.mode, Mode::Current);
        let read = match &mut unit.mode {
            Mode::Current => self
                .source
                .as_mut()
                .expect("current source")
                .read_current(output)?,
            Mode::Spooled { file, .. } => match file.read(output) {
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                Err(error) => return Err(error.into()),
            },
            _ => unreachable!("write stage owns current or spooled payload"),
        };
        unit.record(read, size)?;
        if current {
            if let Some(hash) = &mut unit.hash {
                hash.update(&output[..read]);
            }
            add(&mut self.statistics.hashed_payload_bytes, read as u64)?;
        }
        self.filled += read;
        if self.filled == self.frame.len() || read == 0 {
            if self.filled != 0 || unit.frames == 0 {
                self.pack.as_mut().expect("output pack").write(
                    operation,
                    FrameKind::Unit,
                    &self.frame[..self.filled],
                    &self.control,
                )?;
                unit.frames = unit
                    .frames
                    .checked_add(1)
                    .ok_or(BackupError::Invalid("unit frame count overflow"))?;
            }
            self.filled = 0;
            if read == 0 {
                self.stage = Stage::UnitComplete;
            }
        }
        Ok(())
    }

    pub(super) fn complete_unit(&mut self, operation: &BackupOperation<'a>) -> BackupResult<()> {
        let mut unit = self.unit.take().expect("completed unit");
        let (digest, reference) = match unit.mode {
            Mode::Current => (
                unit.complete_digest(self.source.as_ref().expect("selected source"))?,
                Reference::Written(
                    self.pack
                        .as_ref()
                        .expect("written pack")
                        .range(unit.offset, unit.frames),
                ),
            ),
            Mode::Spooled { digest, .. } => (
                digest,
                Reference::Written(
                    self.pack
                        .as_ref()
                        .expect("written pack")
                        .range(unit.offset, unit.frames),
                ),
            ),
            Mode::Reused { digest, span } => (digest, Reference::Reused(span)),
            Mode::Compare { .. } => unreachable!("comparison did not finish"),
        };
        let reused = matches!(reference, Reference::Reused(_));
        self.statistics.payload(unit.metadata.bytes, reused)?;
        if self.pack.is_none() {
            let Reference::Reused(span) = reference else {
                unreachable!("written unit retains pack");
            };
            self.inventory
                .as_mut()
                .expect("inventory")
                .append(&unit.metadata.entry(
                    digest,
                    span,
                    operation.repository.limits,
                    &self.control,
                )?)?;
        } else {
            self.pending.reserve(1)?;
            self.retained = self
                .retained
                .checked_add(unit.metadata.charge()?)
                .ok_or(BackupError::Invalid("image metadata size overflow"))?;
            self.decoded = self
                .decoded
                .checked_add(unit.metadata.bytes)
                .ok_or(BackupError::Invalid("pack size overflow"))?;
            self.pending.push(Pending {
                metadata: unit.metadata,
                digest,
                reference,
            })?;
        }
        self.selected = false;
        self.stage = Stage::Select;
        Ok(())
    }
}
