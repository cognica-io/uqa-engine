//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact selected catalog-record identities, without reading table data.

use super::{RelationIdentity, RelationKind};

/// The logical key of a catalog record. Providers own its physical encoding.
#[derive(Clone, Copy)]
pub enum CatalogRecordRef<'a> {
    Relation(RelationKind, &'a RelationIdentity),
    Metadata(&'a str),
}

/// Retain the original history/private identity rather than hashing catalog contents. Equal payload replacements, savepoint branches and unrelated commits remain distinguishable. Compare revisions only for the same logical key.
#[derive(Clone, PartialEq, Eq)]
pub enum CatalogRecordRevision {
    KeyValue(crate::key_value::KeyValueReadRevision),
    Record(crate::mvcc::VisibleRecordRevision),
}

impl std::fmt::Debug for CatalogRecordRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogRecordRevision")
            .finish_non_exhaustive()
    }
}
