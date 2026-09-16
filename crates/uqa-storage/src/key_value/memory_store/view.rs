//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory readers borrow one locked map; graph snapshots own their evaluated data afterward.

use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use crate::StorageBackendResult;

pub(super) struct MemoryRead<'a> {
    pub(super) state: &'a super::MemoryKeyValueState,
    pub(super) control: &'a StorageReadControl,
}

impl KeyValueRead for MemoryRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
    }

    fn revision(&self, _prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control.check()?;
        Ok(KeyValueReadRevision::memory(&self.state.read_revision))
    }

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_budgeted(key, self.control, visit)
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_prefix_after(prefix, None, usize::MAX, self.control, visit)
    }

    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        visit(self.state.map.get(key).map(Vec::as_slice))?;
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
        use std::ops::Bound::{Excluded, Included, Unbounded};
        control.check()?;
        let lower = match after {
            Some(after) if after >= prefix => Excluded(after),
            _ => Included(prefix),
        };
        for (key, value) in self
            .state
            .map
            .range::<[u8], _>((lower, Unbounded))
            .take(limit)
        {
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
        use std::ops::Bound::{Included, Unbounded};
        control.check()?;
        Ok(self
            .state
            .map
            .range::<[u8], _>((Included(prefix), Unbounded))
            .next()
            .is_some_and(|(key, _)| key.starts_with(prefix)))
    }
}
