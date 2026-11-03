//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::objects::ObjectWriter;
use super::{
    directory, encoded_bytes, io, write_frame, BackupError, BackupOperation, BackupResult,
    FrameContext, FrameKind, ObjectId, ObjectSpan, Read, State, StorageReadControl,
};
use crate::backup::ImageDigest;

/// Frame-aligned range returned by a pack writer. It acquires its whole-object
/// digest only after the pack is sealed and durably installed.
#[derive(Clone, Copy, Debug)]
pub struct PackRange {
    object: ObjectId,
    offset: u64,
    bytes: u64,
    frames: u64,
}

/// An immutable installed object; ranges cannot be attached to another object.
#[derive(Clone, Copy, Debug)]
pub struct PackObject {
    object: ObjectId,
    digest: ImageDigest,
    bytes: u64,
}

impl PackObject {
    pub(super) const fn encoded_bytes(self) -> u64 {
        self.bytes
    }

    pub fn span(self, range: PackRange) -> BackupResult<ObjectSpan> {
        if range.object != self.object {
            return Err(BackupError::Invalid("foreign pack range"));
        }
        ObjectSpan::new(
            self.object,
            self.digest,
            self.bytes,
            range.offset,
            range.bytes,
            range.frames,
        )
    }
}

/// Exclusive operation borrow enforces one current pack. Dropping an unfinished
/// pack fails capture; callers cannot append after an I/O, cancellation or limit
/// error, or publish a partially written object.
pub struct BackupPack<'operation, 'repository> {
    operation: &'operation mut BackupOperation<'repository>,
    object: ObjectWriter,
    id: ObjectId,
    finished: bool,
    failed: bool,
}

impl<'repository> BackupOperation<'repository> {
    pub fn begin_pack(
        &mut self,
        control: &StorageReadControl,
    ) -> BackupResult<BackupPack<'_, 'repository>> {
        if !matches!(self.state, State::Capturing) {
            return Err(BackupError::Finished);
        }
        let result = (|| {
            control.cancellation().check()?;
            let id = ObjectId::new()?;
            let name = directory::name(id.as_bytes());
            Ok((
                id,
                directory::create_file(&self.directory, directory::text(&name))?,
            ))
        })();
        match result {
            Ok((id, file)) => Ok(BackupPack {
                operation: self,
                object: ObjectWriter::new(file),
                id,
                finished: false,
                failed: false,
            }),
            Err(error) => {
                self.state = State::Failed;
                Err(error)
            }
        }
    }
}

impl BackupPack<'_, '_> {
    /// Append a complete unit or inventory without holding repository admission.
    /// Small units share the pack while retaining independent frame boundaries.
    pub fn append(
        &mut self,
        kind: FrameKind,
        input: &mut impl Read,
        control: &StorageReadControl,
    ) -> BackupResult<PackRange> {
        if self.failed {
            return Err(BackupError::Finished);
        }
        let result = self.append_inner(kind, input, control);
        if result.is_err() {
            self.failed = true;
            self.operation.state = State::Failed;
        }
        result
    }

    fn append_inner(
        &mut self,
        kind: FrameKind,
        input: &mut impl Read,
        control: &StorageReadControl,
    ) -> BackupResult<PackRange> {
        if !matches!(kind, FrameKind::Unit | FrameKind::Inventory) {
            return Err(BackupError::Invalid("capture object kind"));
        }
        let offset = self.object.bytes();
        let mut buffer = [0; 16 * 1024];
        let repository = self.operation.repository;
        let window = buffer
            .len()
            .min(repository.limits.max_frame_bytes() as usize);
        let mut frames = 0;
        loop {
            let mut length = 0;
            while length < window {
                control.cancellation().check()?;
                match input.read(&mut buffer[length..window]) {
                    Ok(0) => break,
                    Ok(read) => length += read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if length == 0 && frames != 0 {
                break;
            }
            let context = FrameContext {
                repository: repository.id,
                object: self.id,
                offset: self.object.bytes(),
                kind,
            };
            self.operation.reserve_staging(encoded_bytes(
                context,
                &repository.protection,
                length,
            )?)?;
            write_frame(
                &mut self.object,
                context,
                &repository.protection,
                &buffer[..length],
                repository.limits,
                control,
            )?;
            frames += 1;
            if length < window {
                break;
            }
        }
        Ok(PackRange {
            object: self.id,
            offset,
            bytes: self.object.bytes() - offset,
            frames,
        })
    }

    pub fn finish(mut self, control: &StorageReadControl) -> BackupResult<PackObject> {
        if self.failed {
            return Err(BackupError::Finished);
        }
        if self.object.bytes() == 0 {
            return Err(BackupError::Invalid("empty pack has no frames"));
        }
        control.cancellation().check()?;
        #[cfg(test)]
        self.operation
            .repository
            .before(super::super::Fault::ObjectSync)?;
        self.object.file.sync_all()?;
        directory::synchronize(&self.operation.directory)?;
        let repository = self.operation.repository;
        let _admission = repository.admission(control)?;
        repository.advance_generation(control)?;
        let name = directory::name(self.id.as_bytes());
        #[cfg(test)]
        repository.before(super::super::Fault::ObjectInstall)?;
        self.operation.directory.hard_link(
            directory::text(&name),
            &repository.objects,
            directory::text(&name),
        )?;
        directory::synchronize(&repository.objects)?;
        self.finished = true;
        Ok(PackObject {
            object: self.id,
            digest: self.object.digest(),
            bytes: self.object.bytes(),
        })
    }
}

impl Drop for BackupPack<'_, '_> {
    fn drop(&mut self) {
        if !self.finished {
            self.operation.state = State::Failed;
        }
    }
}
