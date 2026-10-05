//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The database's OID counter, from which every catalog draws the OIDs of new objects in creation order.

use parking_lot::Mutex;
use std::num::NonZeroU64;
use uqa_sql::SQLError;
use uqa_storage::mvcc::{IdentifierAllocator, IdentifierRequest};

pub use uqa_sql::catalog::oids::FIRST_NORMAL_OBJECT_ID;

/// The storage namespace that keeps the counter's position.
const COUNTER_NAMESPACE: &[u8] = b"catalog-oid\x01";

/// The number of distinct OIDs the counter passes through before it wraps.
const OID_CYCLE: u64 = (1_u64 << 32) - FIRST_NORMAL_OBJECT_ID as u64;

/// `PostgreSQL`'s OID counter (`GetNewObjectId`): one sequence per database shared by every catalog, advanced outside transactions so that a command that rolls back still consumes its OIDs, and wrapping to `FirstNormalObjectId` after the 32-bit space. Storage that reserves identifiers durably keeps the position across sessions, processes and reopening; for storage without that capability the position lives in this value, which every session of the database in the process shares.
#[derive(Debug, Default)]
pub struct CatalogOidCounter {
    position: Mutex<Option<u64>>,
}

impl CatalogOidCounter {
    /// The next OID of the sequence. A sequence that has not started moves past `existing`, the largest OID the catalog already holds, as `PostgreSQL`'s counter is always past every OID it assigned. Callers still skip an OID a catalog holds, as `GetNewOidWithIndex` does after the counter wraps.
    pub fn next_oid(
        &self,
        durable: Option<&dyn IdentifierAllocator>,
        existing: impl FnOnce() -> Result<Option<u32>, SQLError>,
    ) -> Result<u32, SQLError> {
        let position = match durable {
            Some(allocator) => {
                let started = allocator
                    .identifier_watermark(COUNTER_NAMESPACE)
                    .map_err(|error| counter_error(&error))?
                    .is_some();
                if !started {
                    if let Some(oid) = existing()? {
                        allocator
                            .allocate_identifiers(
                                COUNTER_NAMESPACE,
                                IdentifierRequest::Observe(u64::from(oid)),
                            )
                            .map_err(|error| counter_error(&error))?;
                    }
                }
                allocator
                    .allocate_identifiers(
                        COUNTER_NAMESPACE,
                        IdentifierRequest::Reserve {
                            minimum: u64::from(FIRST_NORMAL_OBJECT_ID),
                            maximum: u64::MAX,
                            count: NonZeroU64::MIN,
                        },
                    )
                    .map_err(|error| counter_error(&error))?
                    .watermark()
            }
            None => {
                let mut position = self.position.lock();
                let current = match *position {
                    Some(current) => current,
                    None => existing()?.map_or(0, u64::from),
                };
                let next = current
                    .checked_add(1)
                    .ok_or_else(|| {
                        SQLError::Internal("the catalog OID counter is exhausted".into())
                    })?
                    .max(u64::from(FIRST_NORMAL_OBJECT_ID));
                *position = Some(next);
                next
            }
        };
        Ok(oid_at(position))
    }
}

/// The OID at a position of the sequence: positions past the 32-bit space wrap to `FirstNormalObjectId`.
fn oid_at(position: u64) -> u32 {
    let offset = (position - u64::from(FIRST_NORMAL_OBJECT_ID)) % OID_CYCLE;
    u32::try_from(u64::from(FIRST_NORMAL_OBJECT_ID) + offset)
        .expect("a counter position maps into the 32-bit OID space")
}

fn counter_error(error: &uqa_storage::StorageBackendError) -> SQLError {
    SQLError::Internal(format!("advance the catalog OID counter: {error}"))
}

#[cfg(test)]
mod tests;
