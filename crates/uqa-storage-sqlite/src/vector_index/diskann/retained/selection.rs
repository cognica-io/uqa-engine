//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RetainedSQLiteDiskANNCanonical};
use std::sync::Arc;
use uqa_storage::{
    diskann_index::catalog::DiskANNIndexResolver, key_value::KeyValueDiskANNSource,
    read_control::StorageReadControl, StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    /// Select physical graph data on this native canonical/catalog view, retaining private publication resources without advancing the SQL snapshot or loading page/PQ bodies.
    pub fn selected_source(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<KeyValueDiskANNSource>>> {
        self.check(control)?;
        let scope = self.index_scope(resolver, control)?;
        let parameters = self
            .index_parameters()
            .ok_or_else(|| invalid("missing index parameters"))?;
        let records = self.snapshot.record_read();
        let physical = crate::diskann::map_read(&records, self.snapshot.database)?;
        let source = KeyValueDiskANNSource::select(
            &scope,
            self.dimensions,
            parameters,
            &physical,
            &self.control,
            control,
        )?;
        self.check(control)?;
        Ok(source)
    }
}
