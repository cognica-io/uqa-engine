//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{identity::validate_mapping, invalid, publication, KeyValueDiskANNSource};
use crate::diskann_index::{
    catalog::DiskANNIndexScope,
    changes::{DiskANNChangeJournal, DiskANNChangeRead, DiskANNPruneRequest, DiskANNPruneResult},
    maintenance::{DiskANNStatisticsPage, DiskANNStatisticsRequest},
    pages::DiskANNOriginReader,
};
use crate::key_value::KeyValueRead;
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};
use std::sync::Arc;

/// Complete origin evidence retained from an actual immutable provider source. Verification occurs once per handle; bounded pruning pages reuse point lookup without rescanning the artifact or retaining build input.
pub struct KeyValueDiskANNPruner {
    source: Arc<KeyValueDiskANNSource>,
    origins: DiskANNOriginReader,
    control: StorageReadControl,
}

impl KeyValueDiskANNPruner {
    /// Examine exact outstanding changes on the provider's single captured canonical/journal view under this selected generation. This does not stage writes or grant publication authority.
    pub fn statistics(
        &self,
        journal: &dyn DiskANNChangeRead,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        self.control.check()?;
        let page =
            crate::diskann_index::maintenance::measure(&self.origins, journal, request, control)?;
        self.control.check()?;
        Ok(page)
    }

    pub fn open(
        source: Arc<KeyValueDiskANNSource>,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let origins = DiskANNOriginReader::open(source.clone(), maximum, control)?;
        Ok(Self {
            source,
            origins,
            control: control.clone(),
        })
    }

    /// Provider boundary: require this actual source's generation to remain the committed selected head in the supplied mutation. The provider must also guard its captured catalog binding and use this same mutation for journal deletion.
    pub fn require_selected(
        &self,
        scope: &DiskANNIndexScope,
        dimensions: u32,
        parameters: crate::vector_index::DiskANNIndexParams,
        read: &dyn KeyValueRead,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        let input = self.origins.manifest().input();
        if input.dimensions != dimensions || input.parameters != parameters {
            return Err(invalid(
                "selected generation differs from the current index configuration",
            ));
        }
        let generation = input.generation;
        self.source.check(control)?;
        validate_mapping(scope, generation, &*self.source.read, control)?;
        if publication::selected_generation(scope, read, control)? != Some(generation) {
            return Err(invalid("journal pruning requires the selected generation"));
        }
        let key = publication::head_key(scope);
        if read
            .record_revision(&key)?
            .is_none_or(|revision| revision.has_private_changes())
        {
            return Err(invalid("journal pruning requires a committed head"));
        }
        batch.require_unchanged(&key)?;
        self.control.check()?;
        control.check()
    }

    /// Evaluate bounded exact-key deletions after `require_selected` and the provider's catalog/private-input guards. On any error the enclosing mutation must discard all partial operations. Advance the returned cursor only after durable commit.
    pub fn prune(
        &self,
        journal: &mut dyn DiskANNChangeJournal,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        self.control.check()?;
        let result =
            crate::diskann_index::changes::prune(&self.origins, journal, request, control)?;
        self.control.check()?;
        Ok(result)
    }
}
