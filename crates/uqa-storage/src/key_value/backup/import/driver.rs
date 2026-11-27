//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verified Key/Value images activate only after the last private write transaction is durable.

use super::KeyValueImageImportReader;
use crate::{
    backup::{
        BackupError, BackupImageImportStep, BackupRecordImageWriter, BackupResult, BackupRoot,
        BackupSourceStep, RecordImageImportProgress,
    },
    mvcc::DatabaseRestore,
    read_control::StorageReadControl,
};

pub trait KeyValueImageWriter: BackupRecordImageWriter {
    fn activate(
        self,
        root: BackupRoot,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()>;
}

/// The reader and writer share a terminal lifetime. An unsuccessful import releases both, leaving the private file unavailable for ordinary opens.
pub struct KeyValueImageImportDriver<'a, W> {
    active: Option<(KeyValueImageImportReader<'a>, W)>,
    progress: RecordImageImportProgress,
    request: DatabaseRestore,
    control: StorageReadControl,
    value_limit: usize,
    resource: &'static str,
    complete: bool,
}

impl<'a, W: KeyValueImageWriter> KeyValueImageImportDriver<'a, W> {
    pub fn new(
        input: KeyValueImageImportReader<'a>,
        writer: W,
        request: DatabaseRestore,
        value_limit: usize,
        resource: &'static str,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            active: Some((input, writer)),
            progress: RecordImageImportProgress::default(),
            request,
            control: control.clone(),
            value_limit,
            resource,
            complete: false,
        }
    }

    pub const fn imported_records(&self) -> u64 {
        self.progress.records()
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.progress.identifiers()
    }

    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        if self.complete {
            return Ok(BackupImageImportStep::Imported);
        }
        let step = self.advance_active(maximum_object_bytes);
        if step.is_err() {
            self.active = None;
        }
        step
    }

    fn advance_active(&mut self, maximum: usize) -> BackupResult<BackupImageImportStep> {
        let (input, writer) = self.active.as_mut().ok_or(BackupError::Finished)?;
        match input.advance(maximum)? {
            BackupSourceStep::Pending => (),
            BackupSourceStep::Unit => self.progress.write_current(
                &mut input.0,
                writer,
                self.value_limit,
                self.resource,
                &self.control,
            )?,
            BackupSourceStep::Complete => {
                let root = input.root();
                writer.flush(&self.control)?;
                let (_, writer) = self.active.take().expect("verified physical input");
                writer.activate(root, self.request, &self.control)?;
                self.complete = true;
                return Ok(BackupImageImportStep::Imported);
            }
        }
        Ok(BackupImageImportStep::Pending)
    }
}
