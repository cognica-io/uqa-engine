//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact storage names bind nonzero object generations and independent document-ID watermarks.

mod lifecycle;

use uqa_core::memory::BudgetedVec;

use crate::catalog::new_nonzero_catalog_identity;
use crate::document_store::identifiers::{
    inherit_document_ids, legacy_document_id_metadata_key, observe_document_id,
    restored_document_id_watermark,
};
use crate::read_control::StorageReadControl;
use crate::{KeyValueBatch, RelationIdentity, StorageBackendResult, TableSchema};

use super::catalog::keys::relation_key;
use super::codec::{decode_value, document_key_prefix, encode_value, other_error, single_str_key};
use super::{KeyValueRead, TAG_METADATA, TAG_TABLE};

pub(super) use lifecycle::{drop_binding, prepare_table, rename_bindings};

const TAG: u8 = b'O';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) object: [u8; 16],
    pub(super) generation: [u8; 16],
    pub(super) catalog: bool,
}

impl Owner {
    fn new(object: [u8; 16], generation: [u8; 16], catalog: bool) -> StorageBackendResult<Self> {
        Ok(Self {
            object: if object == [0; 16] {
                new_nonzero_catalog_identity("table", "object")?
            } else {
                object
            },
            generation: if generation == [0; 16] {
                new_nonzero_catalog_identity("table", "storage generation")?
            } else {
                generation
            },
            catalog,
        })
    }

    fn decode(value: &[u8]) -> StorageBackendResult<Self> {
        if value.len() != 34 || value[0] != 1 || value[1] > 1 {
            return Err(other_error("invalid KeyValue table owner encoding"));
        }
        let object: [u8; 16] = value[2..18].try_into().expect("checked owner length");
        let generation: [u8; 16] = value[18..34].try_into().expect("checked owner length");
        if object == [0; 16] || generation == [0; 16] {
            return Err(other_error(
                "KeyValue table owner identities must not be zero",
            ));
        }
        Ok(Self {
            object,
            generation,
            catalog: value[1] == 1,
        })
    }

    fn encode(self) -> [u8; 34] {
        let mut value = [0; 34];
        value[0] = 1;
        value[1] = u8::from(self.catalog);
        value[2..18].copy_from_slice(&self.object);
        value[18..].copy_from_slice(&self.generation);
        value
    }

    fn marker(self) -> [u8; 34] {
        let mut key = self.encode();
        key[..2].copy_from_slice(&[TAG, 1]);
        key
    }

    fn identity_key(self) -> [u8; 18] {
        let mut key = [0; 18];
        key[..2].copy_from_slice(&[TAG, 2]);
        key[2..].copy_from_slice(&self.object);
        key
    }

    pub(super) fn observe(
        self,
        batch: &mut dyn KeyValueBatch,
        id: u64,
    ) -> StorageBackendResult<()> {
        observe_document_id(batch, self.object, self.generation, id)
    }

    pub(super) fn fence(self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        batch.fence_record(&self.marker())
    }

    fn inherit(self, batch: &mut dyn KeyValueBatch, from: Self) -> StorageBackendResult<()> {
        if (self.object, self.generation) != (from.object, from.generation) {
            inherit_document_ids(
                batch,
                (from.object, from.generation),
                (self.object, self.generation),
            )?;
        }
        Ok(())
    }
}

fn binding_key(name: &str, control: &StorageReadControl) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut key = BudgetedVec::new(control.memory());
    key.extend_from_slice(&[TAG, 0])?;
    key.extend_from_slice(name.as_bytes())?;
    Ok(key)
}

fn catalog_key(name: &str) -> StorageBackendResult<Option<Vec<u8>>> {
    let Ok(relation) = RelationIdentity::from_legacy_name(name) else {
        return Ok(None);
    };
    if relation.qualified_name() != name {
        return Ok(None);
    }
    relation_key(TAG_TABLE, &relation).map(Some)
}

fn lookup(read: &dyn KeyValueRead, name: &str) -> StorageBackendResult<Option<Owner>> {
    let key = binding_key(name, read.control())?;
    let mut owner = None;
    read.visit_value(&key, &mut |value| {
        owner = value.map(Owner::decode).transpose()?;
        Ok(())
    })?;
    if let Some(owner) = owner {
        let indexed = read.get(&owner.identity_key())?;
        if indexed.as_deref() != Some(name.as_bytes()) {
            return Err(other_error(
                "table owner identity index disagrees with its binding",
            ));
        }
        let schema = catalog_key(name)?
            .map(|key| read.get(&key))
            .transpose()?
            .flatten();
        if owner.catalog != schema.is_some() {
            return Err(other_error(
                "table owner binding disagrees with its catalog definition",
            ));
        }
        if let Some(bytes) = schema {
            let schema: TableSchema = decode_value(&bytes)?;
            if schema.relation.qualified_name() != name
                || schema.object_id != owner.object
                || schema.storage_generation != owner.generation
            {
                return Err(other_error(
                    "table definition disagrees with its owner identities",
                ));
            }
        }
    }
    Ok(owner)
}

/// Validate a present binding against the catalog on the same view; predecessor definitions may not yet have bindings.
pub(super) fn validate_table(
    read: &dyn KeyValueRead,
    schema: &TableSchema,
) -> StorageBackendResult<()> {
    lookup(read, &schema.relation.qualified_name()).map(|_| ())
}

fn claim(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    owner: Owner,
    previous_name: Option<&str>,
) -> StorageBackendResult<()> {
    let key = owner.identity_key();
    if let Some(existing) = read.get(&key)? {
        if &*existing != name.as_bytes()
            && previous_name.is_none_or(|old| &*existing != old.as_bytes())
        {
            return Err(other_error(
                "table object identity already belongs to another storage name",
            ));
        }
    } else {
        // Predecessor files have table definitions before their owner indexes are introduced. Validate those definitions before making a new identity claim.
        read.visit_prefix(&[TAG_TABLE], &mut |_, value| {
            let _decoded = read.control().memory().reserve(
                value
                    .len()
                    .checked_mul(16)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
            )?;
            let schema: TableSchema = decode_value(value)?;
            let existing = schema.relation.qualified_name();
            if schema.object_id == owner.object
                && existing != name
                && previous_name.is_none_or(|old| old != existing)
            {
                return Err(other_error(
                    "table object identity already belongs to another catalog relation",
                ));
            }
            Ok(())
        })?;
    }
    batch.put(&key, name.as_bytes())?;
    batch.put(&binding_key(name, read.control())?, &owner.encode())
}

fn document_maximum(read: &dyn KeyValueRead, name: &str) -> StorageBackendResult<u64> {
    let prefix = document_key_prefix(name)?;
    let mut maximum = 0;
    read.visit_keys_after(&prefix, None, usize::MAX, read.control(), &mut |key| {
        let id: [u8; 8] = key[prefix.len()..]
            .try_into()
            .map_err(|_| other_error("invalid document identity key"))?;
        maximum = maximum.max(u64::from_be_bytes(id));
        Ok(())
    })?;
    Ok(maximum)
}

fn seed_generation(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    owner: Owner,
) -> StorageBackendResult<()> {
    owner.observe(batch, document_maximum(read, name)?)
}

fn seed(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    owner: Owner,
) -> StorageBackendResult<()> {
    let maximum = document_maximum(read, name)?;
    let legacy_key = single_str_key(TAG_METADATA, &legacy_document_id_metadata_key(name))?;
    let mut legacy = None;
    read.visit_value(&legacy_key, &mut |value| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            legacy = Some(
                std::str::from_utf8(value)
                    .map_err(|error| {
                        other_error(format!(
                            "invalid persisted next id for table `{name}`: {error}"
                        ))
                    })?
                    .parse::<u128>()
                    .map_err(|error| {
                        other_error(format!(
                            "invalid persisted next id for table `{name}`: {error}"
                        ))
                    })?,
            );
        }
        Ok(())
    })?;
    let maximum = u64::try_from(restored_document_id_watermark(maximum, legacy) - 1)
        .map_err(|_| other_error("invalid document id watermark"))?;
    owner.observe(batch, maximum)?;
    if legacy.is_some() {
        batch.put(&legacy_key, b"")?;
    }
    Ok(())
}

/// Select the same owner as the evaluated document read, retaining definition requirements and a mergeable data revision.
pub(super) fn document_owner(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
) -> StorageBackendResult<Owner> {
    let binding = binding_key(name, read.control())?;
    let definition = catalog_key(name)?;
    let owner = if let Some(owner) = lookup(read, name)? {
        batch.require_unchanged(&owner.identity_key())?;
        owner
    } else {
        let schema = definition
            .as_ref()
            .map(|key| read.get(key))
            .transpose()?
            .flatten();
        let owner = if let Some(bytes) = schema {
            let mut schema: TableSchema = decode_value(&bytes)?;
            if schema.relation.qualified_name() != name {
                return Err(other_error(
                    "table definition disagrees with its storage name",
                ));
            }
            let owner = Owner::new(schema.object_id, schema.storage_generation, true)?;
            if schema.object_id != owner.object || schema.storage_generation != owner.generation {
                schema.object_id = owner.object;
                schema.storage_generation = owner.generation;
                batch.put(
                    definition.as_ref().expect("schema has a definition key"),
                    &encode_value(&schema)?,
                )?;
            }
            owner
        } else {
            Owner::new([0; 16], [0; 16], false)?
        };
        seed(read, batch, name, owner)?;
        claim(read, batch, name, owner, None)?;
        owner
    };
    batch.require_unchanged(&binding)?;
    if let Some(key) = definition {
        batch.require_unchanged(&key)?;
    } else if owner.catalog {
        return Err(other_error("catalog-owned storage name is not canonical"));
    }
    batch.touch_marker(&owner.marker(), &[1])?;
    Ok(owner)
}

pub(super) fn fence_data(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
) -> StorageBackendResult<Option<Owner>> {
    let owner = lookup(read, name)?;
    batch.fence_record(&binding_key(name, read.control())?)?;
    if let Some(owner) = owner {
        batch.fence_record(&owner.marker())?;
    }
    Ok(owner)
}

/// Clearing rows preserves the owner and its previously reserved identities, including legacy rows whose highest identity is about to disappear.
pub(super) fn preserve_and_fence(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
) -> StorageBackendResult<()> {
    let schema_exists = catalog_key(name)?
        .map(|key| read.get(&key))
        .transpose()?
        .flatten()
        .is_some();
    if lookup(read, name)?.is_some()
        || schema_exists
        || read.contains_prefix_budgeted(&document_key_prefix(name)?, read.control())?
        || read
            .get(&single_str_key(
                TAG_METADATA,
                &legacy_document_id_metadata_key(name),
            )?)?
            .is_some_and(|value| !value.is_empty())
    {
        document_owner(read, batch, name)?.fence(batch)?;
    }
    batch.fence_record(&binding_key(name, read.control())?)
}
