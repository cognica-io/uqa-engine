//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema ownership and typed relation claims.

use super::{
    decode_value, encode_value, relation_key, single_str_key, KeyValueBatch, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult, StoredRelation,
    TAG_RELATION, TAG_SCHEMA,
};

impl KeyValueCatalog {
    pub(super) fn ensure_schema_exists(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        if self
            .store
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
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        self.ensure_schema_exists(relation)?;
        let key = relation_key(TAG_RELATION, relation)?;
        if let Some(value) = self.store.get(&key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != kind {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists as {}",
                    relation.qualified_name(),
                    existing.as_str()
                )));
            }
        } else {
            batch.put(&key, &encode_value(&StoredRelation { kind })?)?;
        }
        Ok(())
    }

    pub(super) fn release_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        let key = relation_key(TAG_RELATION, relation)?;
        if let Some(value) = self.store.get(&key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != kind {
                return Err(StorageBackendError::Other(format!(
                    "catalog relation `{}` is {}, not {}",
                    relation.qualified_name(),
                    existing.as_str(),
                    kind.as_str()
                )));
            }
            batch.delete(&key)?;
        }
        Ok(())
    }
}
