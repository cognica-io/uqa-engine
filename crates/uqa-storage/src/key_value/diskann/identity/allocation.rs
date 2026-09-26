//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mapping publication and the first owned generation share one evaluated transaction.

use std::num::NonZeroU64;

use crate::diskann_index::catalog::DiskANNIndexScope;
use crate::mvcc::{IdentifierRequest, VersionError};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageBackendResult};

use super::super::{
    invalid, read_data_identity, stored_data_identity, KeyValueDiskANNStage, KeyValueDiskANNStore,
};
use super::Keys;

impl KeyValueDiskANNStore {
    pub(in crate::key_value::diskann) fn allocate_catalog_stage(
        &self,
        scope: &DiskANNIndexScope,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStage> {
        loop {
            let (database, keys, observed) = {
                let _writer = self.owner.writer.lock();
                self.idle(control)?;
                scope.check(self.owner.database, control)?;
                let database = stored_data_identity(&*self.owner.store, control)?;
                let keys = Keys::new(database, scope);
                let mut observed = [None; 2];
                self.owner.store.with_read_view(&mut |read| {
                    observed = keys.read(read, control)?;
                    Ok(())
                })?;
                (database, keys, observed)
            };
            let [table, index] = self.reserve_handles(&keys, observed)?;
            let mut stage = self.allocate_stage(table, index, control)?;
            if stage.generation().database() != database {
                return Err(invalid(
                    "data identity changed during bound generation allocation",
                ));
            }
            let lease = stage.lease.clone();
            let mut changed = false;
            let result = self.mutate_owned(lease.as_ref(), control, &mut |read, batch| {
                scope.check(self.owner.database, control)?;
                if read_data_identity(read, control)? != Some(database) {
                    return Err(invalid("catalog handles belong to another data identity"));
                }
                if keys.read(read, control)? != observed {
                    changed = true;
                    return Ok(());
                }
                for (position, key) in [&keys.table[..], &keys.index[..]].into_iter().enumerate() {
                    batch.require_unchanged(key)?;
                    if observed[position].is_none() {
                        let mut bytes = [1; 9];
                        bytes[1..].copy_from_slice(&[table, index][position].to_be_bytes());
                        batch.put(key, &bytes)?;
                    }
                }
                let revision = stage.generation().generation().to_be_bytes();
                batch.put(&keys.index_guard, &revision)?;
                if observed[1].is_none() {
                    batch.put(&keys.table_guard, &revision)?;
                }
                stage.start_in(read, batch, control)?;
                scope.check(self.owner.database, control)
            });
            match result {
                Err(error) if rejected_mapping_conflict(&error) => {
                    self.rollback_pending()?;
                }
                Err(error) => return Err(error),
                Ok(()) if changed => {}
                Ok(()) => return Ok(stage),
            }
        }
    }

    fn reserve_handles(
        &self,
        keys: &Keys,
        observed: [Option<u64>; 2],
    ) -> StorageBackendResult<[u64; 2]> {
        let Some(count) = NonZeroU64::new(observed.iter().filter(|id| id.is_none()).count() as u64)
        else {
            return Ok(observed.map(|id| id.expect("both handles present")));
        };
        let allocation = self
            .owner
            .store
            .identifier_allocator()
            .ok_or_else(|| invalid("catalog handles require durable identifiers"))?
            .allocate_identifiers(
                &keys.allocator,
                IdentifierRequest::Reserve {
                    minimum: 1,
                    maximum: u64::MAX,
                    count,
                },
            )?;
        let mut next = allocation.watermark() - (count.get() - 1);
        Ok(observed.map(|id| {
            id.unwrap_or_else(|| {
                let id = next;
                next = next.saturating_add(1);
                id
            })
        }))
    }
}

// Only a definite rejected metadata attempt may be reevaluated. Unknown outcomes retain the original owner and receipt for explicit completion.
pub(super) fn rejected_mapping_conflict(error: &StorageBackendError) -> bool {
    let StorageBackendError::Backend { source, .. } = error else {
        return false;
    };
    matches!(
        source.downcast_ref::<VersionError>(),
        Some(VersionError::WriteConflict { .. } | VersionError::ReadConflict { .. })
    )
}
