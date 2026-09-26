//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RetainedDiskANNCanonical};
use crate::diskann_index::{catalog::DiskANNIndexResolver, DiskANNCanonicalRead};
use crate::key_value::KeyValueDiskANNSource;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;

impl RetainedDiskANNCanonical {
    /// Keep the selected immutable generation with this exact canonical/catalog view. A retained private publication survives savepoint undo and session closure; an absent head remains absent. No vectors, graph pages or PQ batches are loaded by capture.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        self.check_control(control)?;
        let scope = self.index_scope(resolver, control)?;
        let parameters = self
            .index_parameters()
            .ok_or_else(|| invalid("missing index parameters"))?;
        let source = KeyValueDiskANNSource::select(
            &scope,
            self.dimensions,
            parameters,
            &*self.read,
            &self.control,
            control,
        )?;
        self.check_control(control)?;
        Ok(source)
    }
}
