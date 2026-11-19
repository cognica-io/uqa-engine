//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unpublished ordered values spill as one linear construction, not live mutations.

use super::{check, disk, invalid, Map, Record, Root};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{BudgetedSharedMap, MemoryBudget};

pub(crate) struct Builder<V> {
    map: Map<V>,
    pending: Option<Box<disk::Builder>>,
    previous: Option<u128>,
    ordered: bool,
    failed: bool,
}

impl<V: Record> Builder<V> {
    pub(crate) fn new(memory: &MemoryBudget, resident_bytes: usize) -> Self {
        Self {
            map: Map::new(memory, resident_bytes),
            pending: None,
            previous: None,
            ordered: true,
            failed: false,
        }
    }

    /// Unordered input and replacement retain ordinary map semantics. Once an
    /// ordered stream ends, publish its private root before the first mutation.
    pub(crate) fn insert(
        &mut self,
        key: u128,
        value: V,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.insert_from(key, value, control, &mut ()))
    }

    pub(crate) async fn insert_from(
        &mut self,
        key: u128,
        value: V,
        control: Option<&StorageReadControl>,
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        if self.failed {
            return Err(invalid("construction cannot continue after an error"));
        }
        self.failed = true;
        let result = self.insert_inner(key, value, control, work).await;
        self.failed = result.is_err();
        result
    }

    async fn insert_inner(
        &mut self,
        key: u128,
        value: V,
        control: Option<&StorageReadControl>,
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        work.checkpoint().await?;
        check(control)?;
        if self.previous.is_some_and(|previous| key <= previous) {
            self.finish_pending()?;
            self.ordered = false;
        }
        if !self.ordered {
            return self.map.insert_from(key, value, control, work).await;
        }
        if let Some(disk) = &mut self.pending {
            disk.push_from(key, &value, &self.map.memory, work).await?;
        } else if let Some(value) = self.map.insert_resident(key, value)? {
            let mut disk = disk::Builder::with_workspace(
                &self.map.memory,
                self.map
                    .workspace
                    .as_ref()
                    .filter(|owner| std::sync::Arc::strong_count(owner) == 1),
                self.map.file_budget.as_ref(),
            )?;
            for entry in self.map.iter() {
                check(control)?;
                let (key, value) = entry?;
                disk.push_from(key, &*value, &self.map.memory, work).await?;
            }
            disk.push_from(key, &value, &self.map.memory, work).await?;
            self.map.root = Root::Memory(BudgetedSharedMap::new(
                &self.map.memory.child(self.map.resident_bytes),
            ));
            self.pending = Some(Box::new(disk));
        }
        self.previous = Some(key);
        self.map.len += 1;
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<Map<V>> {
        check(control)?;
        if self.failed {
            return Err(invalid("construction cannot continue after an error"));
        }
        self.finish_pending()?;
        check(control)?;
        Ok(self.map)
    }

    fn finish_pending(&mut self) -> StorageBackendResult<()> {
        if let Some(disk) = self.pending.take() {
            self.map.root = Root::Disk(disk.finish()?);
            self.map.workspace = None;
        }
        Ok(())
    }
}
