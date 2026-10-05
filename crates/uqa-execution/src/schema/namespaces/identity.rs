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
    reserve_namespace_tuple(
        &context.tuples,
        context.locks,
        context.catalog,
        context.schemas,
        name,
    )
}

/// Lock `name` and allocate a namespace OID for a row about to be created under it; `ALTER SCHEMA RENAME` holds its destination under such a tuple while the members move.
pub(super) fn reserve_namespace_tuple(
    tuples: &super::locking::SchemaLockContext<'_>,
    locks: &dyn crate::row_locks::shared_objects::SharedObjectLockSession,
    catalog: &dyn super::SchemaSecurityCatalog,
    schemas: &dyn uqa_sql::catalog::security::schema_inquiry::SchemaPrivilegeCatalog,
    name: &str,
) -> Result<SchemaTupleIdentity, SQLError> {
    tuples.catalog_write()?;
    let name_guard = locks.acquire_shared_catalog(
        SharedCatalogLock::Name {
            class_id: SCHEMA_CATALOG_CLASS_ID,
            name,
        },
        RelationLockMode::AccessExclusive,
    )?;
    locks.refresh_shared_catalog()?;
    if catalog.schema_security(name).is_some() {
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message:
                "duplicate key value violates unique constraint \"pg_namespace_nspname_index\""
                    .into(),
        });
    }
    name_guard.retain();
    let oid = crate::catalog::identity::reserve_new_catalog_oid(
        locks,
        SCHEMA_CATALOG_CLASS_ID,
        "schema",
        |oid| Ok(namespace_oid_in_use(schemas, oid)),
    )?;
    new_tuple(oid).map_err(|error| SQLError::Internal(error.to_string()))
}

/// Whether a namespace already holds `oid`: a built-in, created or graph schema, or the session's temporary namespace or its TOAST namespace.
pub fn namespace_oid_in_use(
    catalog: &dyn uqa_sql::catalog::security::schema_inquiry::SchemaPrivilegeCatalog,
    oid: i64,
) -> bool {
    if ["pg_catalog", "information_schema", "ag_catalog"]
        .iter()
        .any(|name| schema_oid(name) == oid)
    {
        return true;
    }
    if catalog.temporary_namespace_oids().is_some_and(|oids| {
        oid == i64::from(oids.namespace) || oid == i64::from(oids.toast_namespace)
    }) {
        return true;
    }
    if catalog
        .schemas()
        .iter()
        .any(|(name, security)| security.namespace_oid(name) == oid)
    {
        return true;
    }
    let graphs = catalog.graphs();
    let in_use = graphs.names().any(|name| graphs.namespace_oid(name) == oid);
    in_use
}
