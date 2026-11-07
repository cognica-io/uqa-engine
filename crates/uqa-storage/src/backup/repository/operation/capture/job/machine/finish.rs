//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inventory publication consumes the same complete verification certificate as manual preparation.

use super::{
    add, BackupError, BackupImageReader, BackupImageSource, BackupOperation, BackupResult,
    CapturePack, Machine, Stage,
};
use crate::backup::{BackupReadStep, FrameKind};
use std::io::{self, Read, Seek, SeekFrom};

impl<'a, S: BackupImageSource> Machine<'a, S> {
    pub(super) fn finish_inventory(
        &mut self,
        operation: &mut BackupOperation<'a>,
    ) -> BackupResult<()> {
        let (mut spool, summary) = self
            .inventory
            .take()
            .expect("completed inventory")
            .finish()?;
        if self.expected.is_some_and(|expected| expected != summary) {
            return Err(BackupError::Integrity);
        }
        spool.seek(SeekFrom::Start(0))?;
        self.summary = Some(summary);
        self.inventory_copy = Some(spool);
        self.pack = Some(CapturePack::new(operation, &self.control)?);
        self.inventory_frames = 0;
        self.filled = 0;
        self.stage = Stage::InventoryWrite;
        Ok(())
    }

    pub(super) fn write_inventory(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_bytes: usize,
    ) -> BackupResult<()> {
        let size = maximum_bytes.min(self.frame.len() - self.filled);
        let read = match self
            .inventory_copy
            .as_mut()
            .expect("inventory spool")
            .read(&mut self.frame[self.filled..self.filled + size])
        {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        self.filled += read;
        if self.filled == self.frame.len() || read == 0 {
            if self.filled != 0 || self.inventory_frames == 0 {
                self.pack.as_mut().expect("inventory pack").write(
                    operation,
                    FrameKind::Inventory,
                    &self.frame[..self.filled],
                    &self.control,
                )?;
                self.inventory_frames = self
                    .inventory_frames
                    .checked_add(1)
                    .ok_or(BackupError::Invalid("inventory frame count overflow"))?;
            }
            self.filled = 0;
            if read == 0 {
                self.stage = Stage::InventorySeal;
            }
        }
        Ok(())
    }

    pub(super) fn seal_inventory(
        &mut self,
        operation: &mut BackupOperation<'a>,
    ) -> BackupResult<()> {
        let mut pack = self.pack.take().expect("inventory pack");
        let range = pack.range(0, self.inventory_frames);
        let object = pack.finish(operation, &self.control)?;
        add(
            &mut self.statistics.written_object_bytes,
            object.encoded_bytes(),
        )?;
        let span = object.span(range)?;
        self.inventory_copy = None;
        let candidate = operation.begin_preparation(
            span,
            self.summary.expect("finished inventory"),
            &self.control,
        )?;
        self.verifier = Some(BackupImageReader::with_file_budget(
            operation.repository,
            candidate.root,
            &self.files,
            &self.control,
        )?);
        self.candidate = Some(candidate);
        self.stage = Stage::Verify;
        Ok(())
    }

    pub(super) fn verify(&mut self, maximum_bytes: usize) -> BackupResult<()> {
        let verifier = self
            .verifier
            .as_mut()
            .expect("verification stage owns reader");
        if self.verify_unit {
            if verifier.read_current(&mut self.frame[..maximum_bytes])? == 0 {
                self.verify_unit = false;
            }
        } else {
            match verifier.advance(maximum_bytes)? {
                BackupReadStep::Unit => self.verify_unit = true,
                BackupReadStep::Pending => (),
                BackupReadStep::Complete => self.stage = Stage::Seal,
            }
        }
        self.verified_object_bytes = verifier.verified_object_bytes();
        Ok(())
    }

    pub(super) fn prepare(&mut self, operation: &mut BackupOperation<'a>) -> BackupResult<()> {
        let verified = self
            .verifier
            .take()
            .expect("complete verifier")
            .verified()
            .ok_or(BackupError::Integrity)?;
        operation.finish_preparation(
            self.candidate.take().expect("prepared candidate"),
            verified,
            &self.control,
        )?;
        self.stage = Stage::Complete;
        Ok(())
    }
}
