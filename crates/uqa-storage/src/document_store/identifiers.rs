//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document identity allocation and migration of the legacy per-name watermark.

use std::num::NonZeroU64;

use crate::mvcc::{IdentifierAllocator, IdentifierRequest};
use crate::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub mod conformance;

#[cfg(test)]
mod tests;

/// A document namespace follows a table's durable object and storage generation through renames. A missing durable allocator retains the caller's serialized in-memory watermark contract.
pub struct DocumentIdAllocator<'a> {
    durable: Option<&'a dyn IdentifierAllocator>,
    namespace: [u8; 41],
}

impl<'a> DocumentIdAllocator<'a> {
    pub fn new(
        durable: Option<&'a dyn IdentifierAllocator>,
        object: [u8; 16],
        generation: [u8; 16],
    ) -> StorageBackendResult<Self> {
        if durable.is_some() && (object == [0; 16] || generation == [0; 16]) {
            return Err(StorageBackendError::Other(
                "document allocation requires nonzero table and storage identities".into(),
            ));
        }
        let mut namespace = [0; 41];
        namespace[..9].copy_from_slice(b"document\x01");
        namespace[9..25].copy_from_slice(&object);
        namespace[25..].copy_from_slice(&generation);
        Ok(Self { durable, namespace })
    }

    pub fn is_durable(&self) -> bool {
        self.durable.is_some()
    }

    /// Reserve one identity at or above the restored local floor. The local cache changes only after successful durable reservation; private rollback cannot reclaim a returned identity.
    pub fn allocate(&self, next: &mut u128) -> StorageBackendResult<u64> {
        let minimum = u64::try_from(*next)
            .map_err(|_| StorageBackendError::Other("document id space is exhausted".into()))?;
        let id = match self.durable {
            Some(allocator) => allocator
                .allocate_identifiers(
                    &self.namespace,
                    IdentifierRequest::Reserve {
                        minimum,
                        maximum: u64::MAX,
                        count: NonZeroU64::MIN,
                    },
                )?
                .watermark(),
            None => minimum,
        };
        *next = u128::from(id) + 1;
        Ok(id)
    }

    /// Observe a supplied document identity before its row is published. The restored floor is included so deleting older rows cannot make their identities available again.
    pub fn observe(&self, next: &mut u128, id: u64) -> StorageBackendResult<()> {
        let mut updated = (*next).max(u128::from(id) + 1);
        self.synchronize(&mut updated)?;
        *next = updated;
        Ok(())
    }

    /// Seed existing data and legacy reservations before exposing a migrated table to new sessions. The one-past-last representation preserves the exhausted full-width domain.
    pub fn synchronize(&self, next: &mut u128) -> StorageBackendResult<()> {
        let observed = next
            .checked_sub(1)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| StorageBackendError::Other("invalid document id watermark".into()))?;
        if let Some(allocator) = self.durable {
            let allocated = allocator
                .allocate_identifiers(&self.namespace, IdentifierRequest::Observe(observed))?;
            *next = u128::from(allocated.watermark()) + 1;
        }
        Ok(())
    }

    /// Persist the restored/allocation floor. Durable allocators retire the legacy name-bound value in the catalog transaction after seeding its independent namespace; serialized backends retain that legacy format.
    pub fn persist(
        &self,
        catalog: &dyn CatalogFacade,
        table: &str,
        next: &mut u128,
    ) -> StorageBackendResult<()> {
        self.synchronize(next)?;
        let key = legacy_document_id_metadata_key(table);
        if self.is_durable() {
            if catalog
                .get_metadata(&key)?
                .is_some_and(|value| !value.is_empty())
            {
                catalog.set_metadata(&key, "")?;
            }
            Ok(())
        } else {
            catalog.set_metadata(&key, &next.to_string())
        }
    }
}

pub fn legacy_document_id_metadata_key(table: &str) -> String {
    format!("uqa.table_next_id.v1:{table}")
}

/// Combine visible rows with a persisted or retained reservation floor without losing the full-width exhaustion sentinel.
pub fn restored_document_id_watermark(maximum: u64, reserved_next: Option<u128>) -> u128 {
    reserved_next.unwrap_or(1).max(u128::from(maximum) + 1)
}

pub fn load_legacy_document_id_watermark(
    catalog: &dyn CatalogFacade,
    table: &str,
) -> StorageBackendResult<Option<u128>> {
    let Some(value) = catalog.get_metadata(&legacy_document_id_metadata_key(table))? else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value.parse::<u128>().map(Some).map_err(|error| {
        StorageBackendError::Other(format!(
            "invalid persisted next id for table `{table}`: {error}"
        ))
    })
}
