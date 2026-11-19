//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The operation's pack writer is shared by synchronous and scheduled capture.

use super::super::super::objects::ObjectWriter;
use super::super::{directory, encoded_bytes, write_frame, FrameContext};
use super::{
    BackupError, BackupOperation, BackupResult, FrameKind, ObjectId, PackObject, PackRange, State,
    StorageReadControl,
};
use std::io::{self, Read};

pub(in crate::backup::repository::operation) struct CapturePack {
    object: ObjectWriter,
    id: ObjectId,
}

impl CapturePack {
    pub(in crate::backup::repository::operation) fn new(
        operation: &mut BackupOperation<'_>,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        if !matches!(operation.state, State::Capturing | State::Imaging) {
            return Err(BackupError::Finished);
        }
        let result = (|| {
            control.check()?;
            let id = ObjectId::new()?;
            let name = directory::name(id.as_bytes());
            let file = directory::create_file(&operation.directory, directory::text(&name))?;
            Ok(Self {
                object: ObjectWriter::new(file),
                id,
            })
        })();
        if result.is_err() {
            operation.state = State::Failed;
        }
        result
    }

    pub(in crate::backup::repository::operation) fn bytes(&self) -> u64 {
        self.object.bytes()
    }

    pub(in crate::backup::repository::operation) fn range(
        &self,
        offset: u64,
        frames: u64,
    ) -> PackRange {
        PackRange {
            object: self.id,
            offset,
            bytes: self.object.bytes() - offset,
            frames,
        }
    }

    /// Write one already bounded frame after admitting its exact encoded charge. No source read or repository admission is held across the caller's next step.
    pub(in crate::backup::repository::operation) fn write(
        &mut self,
        operation: &mut BackupOperation<'_>,
        kind: FrameKind,
        input: &[u8],
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if !matches!(operation.state, State::Capturing | State::Imaging) {
            return Err(BackupError::Finished);
        }
        let result = (|| {
            if !matches!(kind, FrameKind::Unit | FrameKind::Inventory) {
                return Err(BackupError::Invalid("capture object kind"));
            }
            control.check()?;
            let repository = operation.repository;
            let context = FrameContext {
                repository: repository.id,
                object: self.id,
                offset: self.object.bytes(),
                kind,
            };
            operation.reserve_staging(encoded_bytes(
                context,
                &repository.protection,
                input.len(),
            )?)?;
            write_frame(
                &mut self.object,
                context,
                &repository.protection,
                input,
                repository.limits,
                control,
            )
            .map(|_| ())
        })();
        if result.is_err() {
            operation.state = State::Failed;
        }
        result
    }

    pub(in crate::backup::repository::operation) fn append(
        &mut self,
        operation: &mut BackupOperation<'_>,
        kind: FrameKind,
        input: &mut impl Read,
        control: &StorageReadControl,
    ) -> BackupResult<PackRange> {
        if !matches!(operation.state, State::Capturing | State::Imaging) {
            return Err(BackupError::Finished);
        }
        let offset = self.bytes();
        let mut buffer = [0; 16 * 1024];
        let window = buffer
            .len()
            .min(operation.repository.limits.max_frame_bytes() as usize);
        let mut frames = 0;
        loop {
            let mut length = 0;
            while length < window {
                control.check()?;
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
            self.write(operation, kind, &buffer[..length], control)?;
            frames += 1;
            if length < window {
                break;
            }
        }
        Ok(self.range(offset, frames))
    }

    pub(in crate::backup::repository::operation) fn finish(
        &mut self,
        operation: &mut BackupOperation<'_>,
        control: &StorageReadControl,
    ) -> BackupResult<PackObject> {
        if !matches!(operation.state, State::Capturing | State::Imaging) {
            return Err(BackupError::Finished);
        }
        if self.object.bytes() == 0 {
            return Err(BackupError::Invalid("empty pack has no frames"));
        }
        control.check()?;
        #[cfg(test)]
        operation
            .repository
            .before(super::super::super::Fault::ObjectSync)?;
        self.object.file.sync_all()?;
        directory::synchronize(&operation.directory)?;
        let repository = operation.repository;
        let _admission = repository.admission(control)?;
        repository.advance_generation(control)?;
        let name = directory::name(self.id.as_bytes());
        #[cfg(test)]
        repository.before(super::super::super::Fault::ObjectInstall)?;
        operation.directory.hard_link(
            directory::text(&name),
            &repository.objects,
            directory::text(&name),
        )?;
        directory::synchronize(&repository.objects)?;
        Ok(PackObject {
            object: self.id,
            digest: self.object.digest(),
            bytes: self.object.bytes(),
        })
    }
}
