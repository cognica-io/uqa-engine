//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve a catalog address against committed rows and competing uncommitted allocations.

use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode,
};
use uqa_sql::SQLError;

/// Keep the selected address until the caller's transaction or savepoint ends. A collision found after waiting releases its candidate lock before trying another address.
pub fn reserve_catalog_oid(
    session: &dyn SharedObjectLockSession,
    class_id: u32,
    kind: &str,
    mut in_use: impl FnMut(i64) -> Result<bool, SQLError>,
    mut allocate: impl FnMut() -> Result<i64, SQLError>,
) -> Result<i64, SQLError> {
    loop {
        let oid = allocate()?;
        let public_oid = u32::try_from(oid)
            .ok()
            .filter(|oid| *oid >= 16_384)
            .ok_or_else(|| SQLError::Internal(format!("invalid {kind} OID allocation")))?;
        if in_use(oid)? {
            continue;
        }
        let guard = session.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id,
                oid: public_oid,
            },
            RelationLockMode::AccessExclusive,
        )?;
        session.refresh_shared_catalog()?;
        if in_use(oid)? {
            continue;
        }
        guard.retain();
        return Ok(oid);
    }
}

#[cfg(test)]
mod tests;
