//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog bootstrap, legacy namespace migration, and schema-shape repair.

mod access;
mod registry;
mod shape;
mod steps;

use super::{RelationIdentity, Result, SQLiteError};

pub(super) fn encode_catalog_id(kind: &str, id: u64) -> Result<i64> {
    i64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} id {id} exceeds the SQLite INTEGER range"))
    })
}

pub(super) fn decode_catalog_id(kind: &str, id: i64) -> Result<u64> {
    u64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt catalog: negative {kind} id {id}"))
    })
}

pub(super) fn migration_relation(value: &str) -> Result<RelationIdentity> {
    steps::v17::migration_relation(value)
}
