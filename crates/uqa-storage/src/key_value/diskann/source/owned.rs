//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private publication retains the read itself, so its build lease travels with that read.

use std::sync::Arc;

use uqa_core::memory::Budgeted;

use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::mvcc::ResourceLease;
use crate::read_control::{
    KeyReadVisitor, KeyValueReadVisitor, StorageReadControl, ValueReadVisitor,
};
use crate::StorageBackendResult;

pub(super) struct OwnedRead {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    lease: ResourceLease,
}

impl OwnedRead {
    pub(super) fn retain(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        lease: ResourceLease,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
        Ok(
            Budgeted::new(Self { read, lease }, control.memory().empty_reservation())
                .into_shared()?,
        )
    }
}

impl KeyValueRead for Budgeted<OwnedRead> {
    fn control(&self) -> &StorageReadControl {
        self.read.control()
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.read.revision(prefixes)
    }
    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.read.record_revision(key)
    }
    fn retained_source(
        &self,
        key: &[u8],
    ) -> StorageBackendResult<Option<Arc<dyn KeyValueRead + Send + Sync>>> {
        self.read.retained_source(key)
    }
    fn retain(
        &self,
        prefixes: &[&[u8]],
    ) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
        OwnedRead::retain(
            self.read.retain(prefixes)?,
            self.lease.clone(),
            self.control(),
        )
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read.visit_value(key, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read.visit_prefix(prefix, visit)
    }
    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read.visit_value_budgeted(key, control, visit)
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read
            .visit_value_bounded(key, max_bytes, control, visit)
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read
            .visit_prefix_after(prefix, after, limit, control, visit)
    }
    fn visit_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read
            .visit_keys_after(prefix, after, limit, control, visit)
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.read.contains_prefix_budgeted(prefix, control)
    }
}
