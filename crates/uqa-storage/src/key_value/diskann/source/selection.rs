//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select physical pages from the canonical query boundary, including private publication resources.

use super::{invalid, DiskANNStageStatus, KeyValueDiskANNSource, ROOT};
use crate::diskann_index::{
    catalog::DiskANNIndexScope,
    format::DiskANNManifest,
    pages::{DiskANNPageSource, DiskANNRecordKey},
};
use crate::key_value::{diskann::publication, KeyValueRead};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};
use std::sync::Arc;

impl KeyValueDiskANNSource {
    /// Provider boundary: select the head, configuration and actual physical source on one retained catalog/canonical view. A committed head retains that same view; a private head requires the source attached by its original publication. Both original and invoking controls remain binding. This reads fixed metadata only, leaving graph/PQ/origin batches lazy.
    pub fn select(
        scope: &DiskANNIndexScope,
        dimensions: u32,
        parameters: DiskANNIndexParams,
        read: &dyn KeyValueRead,
        original: &StorageReadControl,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<Self>>> {
        original.check()?;
        let Some(generation) = publication::selected_generation(scope, read, control)? else {
            original.check()?;
            return Ok(None);
        };
        let key = publication::head_key(scope);
        let revision = read
            .record_revision(&key)?
            .ok_or_else(|| invalid("selected head has no record identity"))?;
        let (physical, status) = if revision.has_private_changes() {
            (
                read.retained_source(&key)?
                    .ok_or_else(|| invalid("private head has no retained physical source"))?,
                DiskANNStageStatus::Sealed,
            )
        } else {
            (read.retain(&[ROOT])?, DiskANNStageStatus::Published)
        };
        let source = Self::from_read(physical, generation, status, Some(original), control)?;
        super::super::identity::validate_mapping(scope, generation, &*source.read, control)?;
        source.read_record(
            DiskANNRecordKey::Manifest,
            DiskANNManifest::MAX_ENCODED_BYTES,
            control,
            &mut |bytes| {
                let manifest = DiskANNManifest::decode(generation, bytes, control)?;
                if manifest.origins().is_none()
                    || manifest.input().dimensions != dimensions
                    || manifest.input().parameters != parameters
                {
                    return Err(invalid(
                        "selected generation differs from the captured index configuration",
                    ));
                }
                Ok(())
            },
        )?;
        source.check(control)?;
        Ok(Some(source))
    }
}
