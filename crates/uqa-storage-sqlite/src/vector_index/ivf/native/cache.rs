//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain evaluated IVF state only under the provider's certified post-write visibility.

use super::{load_state, SQLiteIVFIndex};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::{
    identity::{identity, VectorIdentity},
    NativeVectorRead,
};
use crate::{Result, SQLiteError};
use std::sync::Arc;
use uqa_storage::{ivf_index::IVFPreparedMetadata, KeyValueBatch};

const FAMILIES: [Family; 4] = [
    Family::Vectors,
    Family::IVFIndexes,
    Family::IVFCentroids,
    Family::IVFAssignments,
];

pub(in crate::vector_index::ivf) struct CachedState {
    identity: VectorIdentity,
    value: Arc<IVFPreparedMetadata>,
}

impl SQLiteIVFIndex {
    pub(super) fn cached_native_state(
        &self,
        read: &NativeVectorRead<'_>,
    ) -> Result<Arc<IVFPreparedMetadata>> {
        read.snapshot.control.check()?;
        let identity = identity(read, &FAMILIES)?;
        if let Some(cached) = self.native_state.read().as_ref() {
            if identity.as_ref() == Some(&cached.identity) {
                return Ok(Arc::clone(&cached.value));
            }
        }
        *self.native_state.write() = None;
        Ok(Arc::new(load_state(read, self.params, false)?))
    }

    pub(in crate::vector_index::ivf) fn write_native_state(
        &self,
        operation: impl FnOnce(
            &NativeVectorRead<'_>,
            &mut dyn KeyValueBatch,
        ) -> Result<Option<IVFPreparedMetadata>>,
    ) -> Result<bool> {
        if self.persistent.retained.is_some() {
            return Err(SQLiteError::StorageBackend(
                "a retained vector snapshot is read-only".into(),
            ));
        }
        let result = self
            .persistent
            .conn
            .with_native_write_snapshot(|snapshot, batch| {
                operation(&NativeVectorRead::new(snapshot, &self.persistent)?, batch)
            })?;
        let Some((candidate, staged)) = result else {
            return Ok(false);
        };
        *self.native_state.write() = None;
        // Staging has succeeded; cache admission is advisory and cannot turn that success into an error.
        let retained = (|| {
            let value = candidate?;
            let staged = staged?;
            let read = NativeVectorRead::new(&staged.snapshot, &self.persistent).ok()?;
            let mut identity = identity(&read, &FAMILIES).ok()??;
            if let Some(committed) = staged.committed {
                identity = identity.after_uncontended_commit(committed);
            }
            Some(CachedState {
                identity,
                value: Arc::new(value),
            })
        })();
        *self.native_state.write() = retained;
        Ok(true)
    }
}
