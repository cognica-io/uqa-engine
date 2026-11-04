//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge one authenticated parent inventory without retaining its entry corpus.

use super::super::super::{objects::SpanReader, LocalRepository};
use crate::{
    backup::{
        BackupError, BackupResult, BackupRoot, FrameKind, InventoryEntry, InventoryReader,
        UnitAddress,
    },
    read_control::StorageReadControl,
};

pub(super) struct ParentInventory<'a> {
    input: InventoryReader<SpanReader<'a>>,
    root: BackupRoot,
    current: Option<InventoryEntry>,
    pub(super) visited: u64,
}

impl<'a> ParentInventory<'a> {
    pub(super) fn new(
        repository: &'a LocalRepository,
        root: BackupRoot,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let input =
            repository.span_reader(root.inventory(), FrameKind::Inventory, true, control)?;
        let input = InventoryReader::new(input, repository.limits, control)?;
        if input.format() != root.cut().format {
            return Err(BackupError::Invalid("parent inventory image format"));
        }
        let mut parent = Self {
            input,
            root,
            current: None,
            visited: 0,
        };
        parent.advance()?;
        Ok(parent)
    }

    fn advance(&mut self) -> BackupResult<()> {
        self.current = self.input.read_next()?;
        if self.current.is_some() {
            self.visited = self
                .visited
                .checked_add(1)
                .ok_or(BackupError::Invalid("parent inventory count overflow"))?;
        } else if self.input.summary() != Some(self.root.summary()) {
            return Err(BackupError::Integrity);
        }
        Ok(())
    }

    pub(super) fn matching(
        &mut self,
        address: UnitAddress<'_>,
    ) -> BackupResult<Option<&InventoryEntry>> {
        while self
            .current
            .as_ref()
            .is_some_and(|entry| entry.address() < address)
        {
            self.advance()?;
        }
        Ok(self
            .current
            .as_ref()
            .filter(|entry| entry.address() == address))
    }

    pub(super) fn finish(mut self) -> BackupResult<u64> {
        while self.current.is_some() {
            self.advance()?;
        }
        Ok(self.visited)
    }
}
