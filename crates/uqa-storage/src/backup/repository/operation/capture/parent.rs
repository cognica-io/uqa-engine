//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A parent inventory advances one object window or one authenticated entry at a time.

use super::super::super::{
    directory,
    objects::{ObjectVerification, SpanReader},
    LocalRepository,
};
use crate::{
    backup::{BackupError, BackupResult, BackupRoot, FrameKind, InventoryEntry, InventoryReader},
    read_control::StorageReadControl,
};

pub(super) enum ParentStep {
    Pending,
    Entry,
    Complete,
}

pub(super) struct ParentInventory<'a> {
    repository: &'a LocalRepository,
    verification: Option<ObjectVerification>,
    input: Option<InventoryReader<SpanReader<'a>>>,
    root: BackupRoot,
    current: Option<InventoryEntry>,
    control: StorageReadControl,
    complete: bool,
    pub(super) visited: u64,
}

impl<'a> ParentInventory<'a> {
    pub(super) fn new(
        repository: &'a LocalRepository,
        root: BackupRoot,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        let span = root.inventory();
        let name = directory::name(span.object().as_bytes());
        let file = directory::open_file(&repository.objects, directory::text(&name), false)?;
        let verification = ObjectVerification::new(file, span.digest(), span.object_bytes())?;
        Ok(Self {
            repository,
            verification: Some(verification),
            input: None,
            root,
            current: None,
            control: control.clone(),
            complete: false,
            visited: 0,
        })
    }

    pub(super) fn current(&self) -> Option<&InventoryEntry> {
        self.current.as_ref()
    }

    pub(super) fn advance(&mut self, maximum_bytes: usize) -> BackupResult<ParentStep> {
        if self.complete {
            return Ok(ParentStep::Complete);
        }
        self.control.check()?;
        if let Some(verification) = &mut self.verification {
            if verification.advance(maximum_bytes, &self.control)? {
                self.verification = None;
                let input = self.repository.span_reader(
                    self.root.inventory(),
                    FrameKind::Inventory,
                    false,
                    &self.control,
                )?;
                let input = InventoryReader::new(input, self.repository.limits, &self.control)?;
                if input.format() != self.root.cut().format {
                    return Err(BackupError::Invalid("parent inventory image format"));
                }
                self.input = Some(input);
            }
            return Ok(ParentStep::Pending);
        }
        let input = self.input.as_mut().expect("verified parent owns inventory");
        self.current = input.read_next()?;
        if self.current.is_some() {
            self.visited = self
                .visited
                .checked_add(1)
                .ok_or(BackupError::Invalid("parent inventory count overflow"))?;
            Ok(ParentStep::Entry)
        } else {
            if input.summary() != Some(self.root.summary()) {
                return Err(BackupError::Integrity);
            }
            self.complete = true;
            self.input = None;
            Ok(ParentStep::Complete)
        }
    }
}
