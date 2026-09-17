//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema ownership and typed relation claims on a supplied catalog view.

use super::{
    decode_value, encode_value, relation_key, single_str_key, KeyValueBatch, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult, StoredRelation,
    TAG_RELATION, TAG_SCHEMA,
};
use crate::key_value::KeyValueRead;

pub(super) fn require_relation_kind(
    read: &dyn KeyValueRead,
    relation: &RelationIdentity,
    expected: RelationKind,
) -> StorageBackendResult<()> {
    let value = read
        .get(&relation_key(TAG_RELATION, relation)?)?
        .ok_or_else(|| {
            StorageBackendError::Other(format!(
                "catalog relation `{}` has no {} parent",
                relation.qualified_name(),
                expected.as_str()
            ))
        })?;
    let actual = decode_value::<StoredRelation>(&value)?.kind;
    if actual != expected {
        return Err(StorageBackendError::Other(format!(
            "catalog relation `{}` is {}, not {}",
            relation.qualified_name(),
            actual.as_str(),
            expected.as_str()
        )));
    }
    Ok(())
}

pub(super) fn require_schema_exists(
    read: &dyn KeyValueRead,
    relation: &RelationIdentity,
) -> StorageBackendResult<()> {
    if read
        .get(&single_str_key(TAG_SCHEMA, &relation.schema)?)?
        .is_none()
    {
        return Err(StorageBackendError::Other(format!(
            "schema `{}` does not exist for relation `{}`",
            relation.schema,
            relation.qualified_name()
        )));
    }
    Ok(())
}

pub(super) fn claim_relation(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    relation: &RelationIdentity,
    kind: RelationKind,
) -> StorageBackendResult<()> {
    require_schema_exists(read, relation)?;
    let key = relation_key(TAG_RELATION, relation)?;
    if let Some(value) = read.get(&key)? {
        let existing = decode_value::<StoredRelation>(&value)?.kind;
        if existing != kind {
            return Err(StorageBackendError::Other(format!(
                "relation `{}` already exists as {}",
                relation.qualified_name(),
                existing.as_str()
            )));
        }
    }
    batch.put(&key, &encode_value(&StoredRelation { kind })?)
}

pub(super) fn release_relation(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    relation: &RelationIdentity,
    kind: RelationKind,
) -> StorageBackendResult<()> {
    let key = relation_key(TAG_RELATION, relation)?;
    if let Some(value) = read.get(&key)? {
        let existing = decode_value::<StoredRelation>(&value)?.kind;
        if existing != kind {
            return Err(StorageBackendError::Other(format!(
                "catalog relation `{}` is {}, not {}",
                relation.qualified_name(),
                existing.as_str(),
                kind.as_str()
            )));
        }
    }
    batch.delete(&key)
}

impl KeyValueCatalog {
    pub(super) fn require_relation_kind(
        &self,
        relation: &RelationIdentity,
        expected: RelationKind,
    ) -> StorageBackendResult<()> {
        self.store
            .with_read_view(&mut |read| require_relation_kind(read, relation, expected))
    }
    pub(super) fn require_schema_exists(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        self.store
            .with_read_view(&mut |read| require_schema_exists(read, relation))
    }
    pub(super) fn claim_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        self.store
            .with_read_view(&mut |read| claim_relation(read, batch, relation, kind))
    }
    pub(super) fn release_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        self.store
            .with_read_view(&mut |read| release_relation(read, batch, relation, kind))
    }
}
