//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace OID reservations, lifetimes and catalog tuple replacement identities.

use super::SchemaCreationContext;
use crate::{
    catalog::identity::new_nonzero_catalog_identity,
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use uqa_core::catalog_schema::SchemaTupleIdentity;
use uqa_sql::{
    catalog::{oids::schema_oid, security::BoundSchemaSecurity},
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub const SCHEMA_CATALOG_CLASS_ID: u32 = 2615;

pub fn new_tuple(oid: i64) -> StorageBackendResult<SchemaTupleIdentity> {
    if !u32::try_from(oid).is_ok_and(|oid| oid != 0) {
        return Err(uqa_storage::StorageBackendError::Other(
            "invalid schema OID allocation".into(),
        ));
    }
    let object_id = new_nonzero_catalog_identity("schema", "identity")?;
    Ok(SchemaTupleIdentity {
        oid,
        object_id,
        revision: object_id,
    })
}

pub fn replace_tuple(
    current: &BoundSchemaSecurity,
    next: &mut BoundSchemaSecurity,
) -> Result<(), SQLError> {
    let mut tuple = super::locking::tuple(current)?;
    tuple.revision = new_nonzero_catalog_identity("schema", "tuple revision")
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    next.tuple = Some(tuple);
    Ok(())
}

pub(super) fn reserve_creation(
    context: &SchemaCreationContext<'_>,
    name: &str,
) -> Result<SchemaTupleIdentity, SQLError> {
    context.tuples.catalog_write()?;
    let name_guard = context.locks.acquire_shared_catalog(
        SharedCatalogLock::Name {
            class_id: SCHEMA_CATALOG_CLASS_ID,
            name,
        },
        RelationLockMode::AccessExclusive,
    )?;
    context.locks.refresh_shared_catalog()?;
    if context.catalog.schema_security(name).is_some() {
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message:
                "duplicate key value violates unique constraint \"pg_namespace_nspname_index\""
                    .into(),
        });
    }
    name_guard.retain();
    loop {
        let oid = crate::catalog::identity::allocate_catalog_oid("schema")?;
        if oid_in_use(context.schemas, oid) {
            continue;
        }
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: SCHEMA_CATALOG_CLASS_ID,
                oid: u32::try_from(oid).expect("allocated OID fits in u32"),
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.locks.refresh_shared_catalog()?;
        if oid_in_use(context.schemas, oid) {
            continue;
        }
        guard.retain();
        return new_tuple(oid).map_err(|error| SQLError::Internal(error.to_string()));
    }
}

fn oid_in_use(
    catalog: &dyn uqa_sql::catalog::security::schema_inquiry::SchemaPrivilegeCatalog,
    oid: i64,
) -> bool {
    if ["pg_catalog", "information_schema", "ag_catalog"]
        .iter()
        .any(|name| schema_oid(name) == oid)
    {
        return true;
    }
    if catalog
        .schemas()
        .iter()
        .any(|(name, security)| security.namespace_oid(name) == oid)
    {
        return true;
    }
    catalog.graphs().names().any(|name| schema_oid(name) == oid)
}
