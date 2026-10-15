//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit physical catalog versions share the caller's definition transaction.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use uqa_core::catalog_identity::CatalogObjectIdentity;
use uqa_sql::schema::constraint_metadata::CatalogOidClass;
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
    StorageSavepointId,
};

pub mod constraint;
pub mod publication;
mod transaction;

const FORMAT: u32 = 1;
const TUPLE_PREFIX: &str = "uqa.sql.catalog.tuple.v1:";
const HEAD_PREFIX: &str = "uqa.sql.catalog.current.v1:";

/// A physical predecessor selected from current definitions. Its logical OID is not a physical scan key, and replacement never reuses its version, even when every stored field remains equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalTuple {
    class_oid: u32,
    object: CatalogObjectIdentity,
    version: [u8; 16],
    generation: u64,
}

impl PhysicalTuple {
    pub const fn identity(self) -> CatalogObjectIdentity {
        self.object
    }

    /// Replacement order within one immutable logical incarnation. It does not determine snapshot visibility.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    fn validate(self, class: CatalogOidClass) -> StorageBackendResult<()> {
        if self.class_oid != class.class_id() || !self.object.is_valid() || self.version == [0; 16]
        {
            return Err(corrupt("invalid physical catalog tuple identity"));
        }
        Ok(())
    }

    fn key(self) -> String {
        format!(
            "{TUPLE_PREFIX}{:08x}:{:032x}",
            self.class_oid,
            u128::from_be_bytes(self.version)
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record<'a> {
    format: u32,
    tuple: PhysicalTuple,
    #[serde(borrow)]
    payload: &'a RawValue,
}

fn head_key(class: CatalogOidClass, object: CatalogObjectIdentity) -> String {
    format!(
        "{HEAD_PREFIX}{:08x}:{:032x}",
        class.class_id(),
        u128::from_be_bytes(object.object_id)
    )
}

/// Select the current physical predecessor inside one active paired transaction. The caller serializes access to its private writes; head and body reads must not observe separate autocommit boundaries. This is a definition lookup, distinct from visiting rows through a fixed committed/private catalog view.
pub fn current(
    session: &PersistentStorageSession,
    class: CatalogOidClass,
    object: CatalogObjectIdentity,
) -> StorageBackendResult<Option<PhysicalTuple>> {
    transaction::require_active(session)?;
    current_in_transaction(session.catalog.as_ref(), class, object)
}

fn current_in_transaction(
    catalog: &dyn CatalogFacade,
    class: CatalogOidClass,
    object: CatalogObjectIdentity,
) -> StorageBackendResult<Option<PhysicalTuple>> {
    if !object.is_valid() {
        return Err(corrupt("invalid logical catalog identity"));
    }
    let Some(json) = catalog.get_metadata(&head_key(class, object))? else {
        return Ok(None);
    };
    let tuple: PhysicalTuple = decode(&json)?;
    tuple.validate(class)?;
    if tuple.object != object {
        return Err(corrupt("catalog tuple head names another logical object"));
    }
    let key = tuple.key();
    let mut found = false;
    catalog.visit_metadata_with_key_length(&key, key.len(), &mut |key, json| {
        if record(json, key, class)?.tuple != tuple {
            return Err(corrupt(
                "catalog tuple head differs from its physical record",
            ));
        }
        found = true;
        Ok(())
    })?;
    if !found {
        return Err(corrupt("catalog tuple head has no physical record"));
    }
    Ok(Some(tuple))
}

/// Visit the physical bag selected by the supplied catalog snapshot, without collecting records or consulting current heads. The payload stays borrowed until the callback returns; only the consumer decodes selected fields. Logical OIDs may repeat.
pub fn visit(
    catalog: &dyn CatalogFacade,
    class: CatalogOidClass,
    visitor: &mut dyn FnMut(PhysicalTuple, &RawValue) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let prefix = format!("{TUPLE_PREFIX}{:08x}:", class.class_id());
    catalog.visit_metadata_with_key_length(&prefix, prefix.len() + 32, &mut |key, json| {
        let row = record(json, key, class)?;
        visitor(row.tuple, row.payload)
    })
}

/// Insert a new physical row inside an already active paired definition transaction. A conflicting current object is rejected; unrelated rows remain untouched.
pub fn insert(
    session: &PersistentStorageSession,
    class: CatalogOidClass,
    object: CatalogObjectIdentity,
    payload: &RawValue,
) -> StorageBackendResult<PhysicalTuple> {
    publish(session, class, object, None, payload)
}

/// Replace precisely the selected predecessor. Equal payloads still allocate a new physical row; callers implement a true no-op by retaining their existing tuple without calling this operation.
pub fn replace(
    session: &PersistentStorageSession,
    class: CatalogOidClass,
    predecessor: PhysicalTuple,
    payload: &RawValue,
) -> StorageBackendResult<PhysicalTuple> {
    predecessor.validate(class)?;
    publish(
        session,
        class,
        predecessor.object,
        Some(predecessor),
        payload,
    )
}

fn publish(
    session: &PersistentStorageSession,
    class: CatalogOidClass,
    object: CatalogObjectIdentity,
    predecessor: Option<PhysicalTuple>,
    payload: &RawValue,
) -> StorageBackendResult<PhysicalTuple> {
    transaction::atomic(session, || {
        let catalog = session.catalog.as_ref();
        if current_in_transaction(catalog, class, object)? != predecessor {
            return Err(stale());
        }
        let generation = predecessor.map_or(Ok(0), |tuple| {
            tuple
                .generation
                .checked_add(1)
                .ok_or_else(|| diagnostic("54000", "catalog tuple generation exhausted".into()))
        })?;
        let tuple = allocate(catalog, class, object, generation)?;
        let json = serde_json::to_string(&Record {
            format: FORMAT,
            tuple,
            payload,
        })?;
        let head = serde_json::to_string(&tuple)?;
        if let Some(previous) = predecessor {
            catalog.delete_metadata(&previous.key())?;
        }
        catalog.set_metadata(&tuple.key(), &json)?;
        catalog.set_metadata(&head_key(class, object), &head)?;
        Ok(tuple)
    })
}

pub fn remove(
    session: &PersistentStorageSession,
    class: CatalogOidClass,
    predecessor: PhysicalTuple,
) -> StorageBackendResult<()> {
    predecessor.validate(class)?;
    transaction::atomic(session, || {
        let catalog = session.catalog.as_ref();
        if current_in_transaction(catalog, class, predecessor.object)? != Some(predecessor) {
            return Err(stale());
        }
        catalog.delete_metadata(&predecessor.key())?;
        catalog.delete_metadata(&head_key(class, predecessor.object))
    })
}

fn allocate(
    catalog: &dyn CatalogFacade,
    class: CatalogOidClass,
    object: CatalogObjectIdentity,
    generation: u64,
) -> StorageBackendResult<PhysicalTuple> {
    loop {
        let tuple = PhysicalTuple {
            class_oid: class.class_id(),
            object,
            generation,
            version: super::identity::new_nonzero_catalog_identity("catalog", "physical tuple")?,
        };
        if catalog.get_metadata(&tuple.key())?.is_none() {
            return Ok(tuple);
        }
    }
}

fn record<'a>(
    json: &'a str,
    key: &str,
    class: CatalogOidClass,
) -> StorageBackendResult<Record<'a>> {
    let record: Record<'a> = decode(json)?;
    record.tuple.validate(class)?;
    if record.format != FORMAT || record.tuple.key() != key {
        return Err(corrupt(
            "catalog tuple format or physical key does not match",
        ));
    }
    Ok(record)
}

fn decode<'a, T: Deserialize<'a>>(json: &'a str) -> StorageBackendResult<T> {
    serde_json::from_str(json)
        .map_err(|error| corrupt(format!("invalid catalog tuple record: {error}")))
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    diagnostic("XX001", message.into())
}

fn stale() -> StorageBackendError {
    diagnostic(
        "40001",
        "catalog tuple predecessor changed before publication".into(),
    )
}

fn diagnostic(sqlstate: &str, message: String) -> StorageBackendError {
    StorageBackendError::backend(
        "catalog tuple",
        uqa_sql::SQLError::Routine {
            sqlstate: sqlstate.into(),
            message,
        },
    )
}

#[cfg(test)]
mod tests;
