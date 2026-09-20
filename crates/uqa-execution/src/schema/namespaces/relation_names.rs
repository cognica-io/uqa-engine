//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve a relation destination after its statement-level collision preflight.

use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode,
};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

pub const RELATION_CATALOG_CLASS_ID: u32 = 1259;

/// A competitor committed after the ordinary existence check is a catalog uniqueness violation, including for IF NOT EXISTS. Retention follows the caller's transaction and savepoint lifecycle.
pub fn reserve_relation_name(
    session: &dyn SharedObjectLockSession,
    relation: &RelationIdentity,
    in_use: impl FnOnce() -> Result<bool, SQLError>,
) -> Result<(), SQLError> {
    reserve_catalog_name(
        session,
        relation,
        RELATION_CATALOG_CLASS_ID,
        "pg_class_relname_nsp_index",
        in_use,
    )
}

pub(super) fn reserve_catalog_name(
    session: &dyn SharedObjectLockSession,
    identity: &RelationIdentity,
    class_id: u32,
    index: &str,
    in_use: impl FnOnce() -> Result<bool, SQLError>,
) -> Result<(), SQLError> {
    let guard = session.acquire_shared_catalog(
        SharedCatalogLock::Name {
            class_id,
            name: &identity.qualified_name(),
        },
        RelationLockMode::AccessExclusive,
    )?;
    session.refresh_shared_catalog()?;
    if in_use()? {
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message: format!("duplicate key value violates unique constraint \"{index}\""),
        });
    }
    guard.retain();
    Ok(())
}

#[cfg(test)]
mod tests;
