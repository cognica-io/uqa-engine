//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RetainedDiskANNCanonical};
use crate::diskann_index::{build::DiskANNCanonicalCoverage, catalog::DiskANNIndexResolver};
use crate::key_value::{publication, KeyValueRead};
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};

impl RetainedDiskANNCanonical {
    /// Retire this captured head before its SQL catalog row is removed. A competing publication or definition invalidates the operation; retained snapshots remain readable.
    pub fn retire_generation(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<crate::diskann_index::format::DiskANNGeneration> {
        self.require_current_index(read, batch, control)?;
        let scope = self.index_scope(resolver, control)?;
        publication::retire_captured_generation(&scope, &*self.read, read, batch, control)
    }

    /// Publish a completed build in the supplied caller mutation. The read and batch must belong to the same `with_mutation` callback; this method never completes that transaction. The SQL lifecycle must supersede the effect if later private DDL invalidates the index.
    pub fn publish_generation(
        coverage: &DiskANNCanonicalCoverage<Self>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &crate::key_value::KeyValueDiskANNSource,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        coverage.check_control(control)?;
        let source = coverage.source();
        source.require_current_index(read, batch, control)?;
        let prefixes = [&*source.vectors, &*source.origins];
        let original = source.read.revision(&prefixes)?;
        if original.has_private_changes()
            && !original.same_private_changes(&read.revision(&prefixes)?)
        {
            return Err(invalid(
                "captured private canonical input changed before publication",
            ));
        }
        let scope = source.index_scope(resolver, control)?;
        publication::publish_captured_generation(
            coverage,
            &scope,
            source
                .index_parameters()
                .ok_or_else(|| invalid("missing index parameters"))?,
            publication::DiskANNPublicationViews {
                captured: &*source.read,
                current: read,
                batch,
                sealed,
            },
            control,
        )
    }
}
