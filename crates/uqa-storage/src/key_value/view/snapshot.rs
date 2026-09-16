//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded retained copies for readers without a native version owner.

use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedVec};

type Entry = (BudgetedVec<u8>, BudgetedVec<u8>);
use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use crate::StorageBackendResult;

struct RetainedRead {
    rows: Arc<Budgeted<Vec<Entry>>>,
    control: StorageReadControl,
    identity: KeyValueReadRevision,
    _memory: uqa_core::memory::MemoryReservation,
}

pub(super) fn capture<T: KeyValueRead + ?Sized>(
    read: &T,
    prefixes: &[&[u8]],
) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
    read.control().check()?;
    let memory = read
        .control()
        .memory()
        .reserve(std::mem::size_of::<RetainedRead>())?;
    let mut rows = BudgetedVec::new(read.control().memory());
    for prefix in prefixes {
        read.visit_prefix(prefix, &mut |key, value| {
            read.control().check()?;
            rows.reserve(1)?;
            let mut owned_key = BudgetedVec::new(read.control().memory());
            owned_key.extend_from_slice(key)?;
            let mut owned_value = BudgetedVec::new(read.control().memory());
            owned_value.extend_from_slice(value)?;
            rows.push((owned_key, owned_value))?;
            Ok(())
        })?;
    }
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    // Prefix selections may overlap. Keep one immutable copy per identity.
    let (mut rows, rows_memory) = rows.into_parts();
    rows.dedup_by(|a, b| *a.0 == *b.0);
    Ok(Arc::new(RetainedRead {
        rows: Budgeted::new(rows, rows_memory).into_shared()?,
        control: read.control().clone(),
        identity: read.revision(prefixes)?,
        _memory: memory,
    }))
}

impl KeyValueRead for RetainedRead {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }

    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control.check()?;
        Ok(self.identity.clone())
    }

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_budgeted(key, &self.control, visit)
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_prefix_after(prefix, None, usize::MAX, &self.control, visit)
    }

    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let found = self
            .rows
            .binary_search_by(|entry| entry.0.as_ref().cmp(key))
            .ok();
        visit(found.map(|i| &*self.rows[i].1))?;
        control.check()
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let start = self.rows.partition_point(|(key, _)| {
            &**key < prefix || after.is_some_and(|after| &**key <= after)
        });
        for (key, value) in self.rows[start..].iter().take(limit) {
            if !key.starts_with(prefix) {
                break;
            }
            control.check()?;
            visit(key, value)?;
        }
        control.check()
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        let start = self.rows.partition_point(|(key, _)| &**key < prefix);
        Ok(self
            .rows
            .get(start)
            .is_some_and(|(key, _)| key.starts_with(prefix)))
    }
}
