//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed shared-catalog addresses use the existing transaction lock lifecycle.

use super::{RelationLockMode, ScopedRelationLock};
use uqa_sql::SQLError;

#[derive(Clone, Copy, Debug)]
pub enum SharedCatalogLock<'a> {
    Object {
        class_id: u32,
        oid: u32,
    },
    Name {
        class_id: u32,
        name: &'a str,
    },
    MemberName {
        class_id: u32,
        owner_class_id: u32,
        owner_object_id: [u8; 16],
        name: &'a str,
    },
    Tuple {
        class_id: u32,
        oid: u32,
    },
}

pub trait SharedObjectLockSession {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError>;
    fn refresh_shared_catalog(&self) -> Result<(), SQLError>;
    /// The next OID of the database's counter, as [`crate::catalog::identity::CatalogOidCounter::next_oid`] advances it.
    fn next_catalog_oid(&self) -> Result<u32, SQLError>;
}

#[cfg(test)]
mod tests;
