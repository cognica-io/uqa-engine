//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace lifetime binding and original catalog tuple replacement checks.

use super::{identity::SCHEMA_CATALOG_CLASS_ID, SchemaSecurityCatalog};
use crate::row_locks::{
    binding::{acquire_relation, RelationLockSession},
    session::RowLockSession,
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    LockAcquire, RelationLockMode,
};
use uqa_core::catalog_schema::SchemaTupleIdentity;
use uqa_sql::{
    ast::{LockStrength, LockWait},
    catalog::security::BoundSchemaSecurity,
    SQLError,
};

pub struct SchemaLockContext<'a> {
    pub objects: &'a dyn SharedObjectLockSession,
    pub relations: &'a dyn RelationLockSession,
    pub rows: &'a dyn RowLockSession,
    pub catalog: &'a dyn SchemaSecurityCatalog,
}

impl SchemaLockContext<'_> {
    /// A name lookup may bind a replacement namespace after waiting for deletion.
    pub fn bind_lifetime(
        &self,
        name: &str,
        mode: RelationLockMode,
    ) -> Result<Option<BoundSchemaSecurity>, SQLError> {
        bind_namespace_lifetime(self.objects, mode, || {
            self.catalog
                .schema_security(name)
                .map(|security| Ok((tuple(&security)?, security)))
                .transpose()
        })
    }

    pub fn catalog_write(&self) -> Result<(), SQLError> {
        acquire_relation(
            self.relations,
            "pg_catalog.pg_namespace",
            RelationLockMode::RowExclusive,
            false,
        )?
        .retain();
        self.objects.refresh_shared_catalog()
    }

    /// Tuple updates retain their original target even if its name is reused while waiting.
    pub fn replace(&self, name: &str, before: SchemaTupleIdentity) -> Result<(), SQLError> {
        match self.rows.lock_row(
            "pg_catalog.pg_namespace",
            before.oid as u64,
            LockStrength::ForNoKeyUpdate,
            LockWait::Block,
            name,
        )? {
            LockAcquire::Granted { .. } => {}
            LockAcquire::Skipped => {
                return Err(SQLError::Internal(
                    "blocking schema tuple lock was skipped".into(),
                ));
            }
        }
        self.objects.refresh_shared_catalog()?;
        let current = self
            .catalog
            .schema_security(name)
            .map(|row| tuple(&row))
            .transpose()?;
        let Some(current) = current
            .filter(|current| current.oid == before.oid && current.object_id == before.object_id)
        else {
            return Err(concurrent_tuple("deleted"));
        };
        if current.revision != before.revision {
            return Err(concurrent_tuple("updated"));
        }
        Ok(())
    }
}

/// Re-resolve names and authority after each wait before retaining the selected lifetime.
pub(super) fn bind_namespace_lifetime<T>(
    objects: &dyn SharedObjectLockSession,
    mode: RelationLockMode,
    mut resolve: impl FnMut() -> Result<Option<(SchemaTupleIdentity, T)>, SQLError>,
) -> Result<Option<T>, SQLError> {
    loop {
        let Some((before, _)) = resolve()? else {
            return Ok(None);
        };
        let guard = objects.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: SCHEMA_CATALOG_CLASS_ID,
                oid: u32::try_from(before.oid).expect("validated schema OID"),
            },
            mode,
        )?;
        objects.refresh_shared_catalog()?;
        let Some((current, value)) = resolve()? else {
            return Ok(None);
        };
        if current.oid != before.oid || current.object_id != before.object_id {
            continue;
        }
        guard.retain();
        return Ok(Some(value));
    }
}

pub(super) fn tuple(security: &BoundSchemaSecurity) -> Result<SchemaTupleIdentity, SQLError> {
    security
        .tuple
        .filter(|tuple| tuple.is_valid())
        .ok_or_else(|| SQLError::Internal("schema has no valid catalog tuple identity".into()))
}

pub(super) fn missing(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "3F000".into(),
        message: format!("schema \"{name}\" does not exist"),
    }
}

fn concurrent_tuple(action: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!("tuple concurrently {action}"),
    }
}

#[cfg(test)]
mod tests;
