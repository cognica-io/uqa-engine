//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A possible inherited unit is compared before any new repository payload is written.

use std::io::{self, Read, Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{
    parent::ParentInventory, BackupImageSource, BackupImageStatistics, BackupLimits, BackupResult,
    FileBudget, FrameKind, ImageDigest, Pending, Reference, StorageReadControl, TemporaryFile,
    UnitMetadata,
};
use crate::backup::{BackupError, BackupPack, ObjectSpan};

pub(super) enum SelectedImage {
    Current,
    Spooled {
        file: TemporaryFile,
        digest: ImageDigest,
    },
    Reused {
        span: ObjectSpan,
        digest: ImageDigest,
    },
}

pub(super) struct SelectedUnit {
    pub(super) metadata: UnitMetadata,
    pub(super) image: SelectedImage,
}

impl SelectedUnit {
    pub(super) fn new(
        source: &mut impl BackupImageSource,
        parent: Option<&mut ParentInventory<'_>>,
        files: &FileBudget,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        let unit = source
            .current_unit()
            .ok_or(BackupError::Invalid("source has no selected image unit"))?;
        limits.check_key(unit.address.key.len())?;
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(unit.address.key)?;
        let candidate = parent
            .map(|parent| parent.matching(unit.address))
            .transpose()?
            .flatten()
            .filter(|entry| {
                entry.kind() == unit.kind && entry.decoded_bytes() == unit.decoded_bytes
            })
            .map(|entry| (entry.digest(), entry.object()));
        let metadata = UnitMetadata {
            namespace: unit.address.namespace,
            generation: unit.address.generation,
            family: unit.address.family,
            key,
            kind: unit.kind,
            bytes: unit.decoded_bytes,
        };
        let image = match candidate {
            None => SelectedImage::Current,
            Some((expected_digest, span)) => {
                let mut file = TemporaryFile::with_file_budget(files)?;
                let mut input = Payload::new(source, metadata.bytes);
                let mut buffer = [0; 16 * 1024];
                let window = buffer.len().min(limits.max_frame_bytes() as usize);
                loop {
                    control.check()?;
                    let read = input.read_source(&mut buffer[..window])?;
                    if read == 0 {
                        break;
                    }
                    file.write_all(&buffer[..read])?;
                }
                let digest = input.digest();
                if digest == expected_digest {
                    SelectedImage::Reused { span, digest }
                } else {
                    file.seek(SeekFrom::Start(0))?;
                    SelectedImage::Spooled { file, digest }
                }
            }
        };
        Ok(Self { metadata, image })
    }

    pub(super) fn append(
        self,
        pack: &mut BackupPack<'_, '_>,
        source: &mut impl BackupImageSource,
        statistics: &mut BackupImageStatistics,
        control: &StorageReadControl,
    ) -> BackupResult<Pending> {
        let (digest, reference) = match self.image {
            SelectedImage::Current => {
                let mut input = Payload::new(source, self.metadata.bytes);
                let range = pack.append(FrameKind::Unit, &mut input, control)?;
                (input.digest(), Reference::Written(range))
            }
            SelectedImage::Spooled { mut file, digest } => (
                digest,
                Reference::Written(pack.append(FrameKind::Unit, &mut file, control)?),
            ),
            SelectedImage::Reused { span, digest } => (digest, Reference::Reused(span)),
        };
        statistics.payload(
            self.metadata.bytes,
            matches!(reference, Reference::Reused(_)),
        )?;
        Ok(Pending {
            metadata: self.metadata,
            digest,
            reference,
        })
    }
}

struct Payload<'a, S> {
    source: &'a mut S,
    hash: Sha256,
    bytes: u64,
    expected: u64,
}

impl<'a, S: BackupImageSource> Payload<'a, S> {
    fn new(source: &'a mut S, expected: u64) -> Self {
        Self {
            source,
            hash: Sha256::new(),
            bytes: 0,
            expected,
        }
    }

    fn read_source(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let read = self.source.read_current(output)?;
        if read > output.len() {
            return Err(BackupError::Invalid("source read exceeds output buffer"));
        }
        self.bytes = self
            .bytes
            .checked_add(read as u64)
            .ok_or(BackupError::Integrity)?;
        if self.bytes > self.expected || (read == 0 && self.bytes != self.expected) {
            return Err(BackupError::Integrity);
        }
        self.hash.update(&output[..read]);
        Ok(read)
    }

    fn digest(self) -> ImageDigest {
        ImageDigest::from_bytes(self.hash.finalize().into())
    }
}

impl<S: BackupImageSource> Read for Payload<'_, S> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.read_source(output).map_err(io::Error::other)
    }
}
