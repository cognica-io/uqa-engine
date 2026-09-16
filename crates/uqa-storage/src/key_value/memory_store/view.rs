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
        self.control.check()?;
        visit(self.state.map.get(key).map(Vec::as_slice))?;
        self.control.check()
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        use std::ops::Bound::{Included, Unbounded};
        self.control.check()?;
        for (key, value) in self
            .state
            .map
            .range::<[u8], _>((Included(prefix), Unbounded))
        {
            if !key.starts_with(prefix) {
                break;
            }
            self.control.check()?;
            visit(key, value)?;
        }
        self.control.check()
    }
}
