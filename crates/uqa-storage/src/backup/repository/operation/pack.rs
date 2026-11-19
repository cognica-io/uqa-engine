//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BackupError, BackupOperation, BackupResult, FrameKind, ObjectId, ObjectSpan, Read, State,
    StorageReadControl,
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

mod writer;
pub(super) use writer::CapturePack;

/// Exclusive operation borrow enforces one current pack. Dropping an unfinished pack fails capture; callers cannot append after an I/O, cancellation or limit error, or publish a partially written object.
pub struct BackupPack<'operation, 'repository> {
    operation: &'operation mut BackupOperation<'repository>,
    writer: CapturePack,
    finished: bool,
}

impl<'repository> BackupOperation<'repository> {
    pub fn begin_pack(
        &mut self,
        control: &StorageReadControl,
    ) -> BackupResult<BackupPack<'_, 'repository>> {
        if !matches!(self.state, State::Capturing) {
            return Err(BackupError::Finished);
        }
        let writer = CapturePack::new(self, control)?;
        Ok(BackupPack {
            operation: self,
            writer,
            finished: false,
        })
    }
}

impl BackupPack<'_, '_> {
    /// Append a complete unit or inventory without holding repository admission. Small units share the pack while retaining independent frame boundaries.
    pub fn append(
        &mut self,
        kind: FrameKind,
        input: &mut impl Read,
        control: &StorageReadControl,
    ) -> BackupResult<PackRange> {
        let result = self.writer.append(self.operation, kind, input, control);
        if result.is_err() {
            self.operation.state = State::Failed;
        }
        result
    }

    pub fn finish(mut self, control: &StorageReadControl) -> BackupResult<PackObject> {
        let object = self.writer.finish(self.operation, control)?;
        self.finished = true;
        Ok(object)
    }
}

impl Drop for BackupPack<'_, '_> {
    fn drop(&mut self) {
        if !self.finished {
            self.operation.state = State::Failed;
        }
    }
}
