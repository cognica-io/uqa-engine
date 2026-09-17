//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog adoption and exact-name transfers retain or inherit the appropriate reserved document identities.

use super::{binding_key, claim, document_owner, fence_data, lookup, seed, seed_generation, Owner};
use crate::key_value::catalog::keys::relation_key;
use crate::key_value::codec::{decode_value, other_error};
use crate::key_value::{KeyValueRead, TAG_TABLE};
use crate::{KeyValueBatch, RelationIdentity, StorageBackendResult, TableSchema};

fn retire(
    batch: &mut dyn KeyValueBatch,
    name: &str,
    owner: Owner,
    read: &dyn KeyValueRead,
) -> StorageBackendResult<()> {
    batch.fence_record(&owner.marker())?;
    batch.delete(&binding_key(name, read.control())?)?;
    batch.delete(&owner.identity_key())
}

pub(in crate::key_value) fn prepare_table(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    schema: &TableSchema,
) -> StorageBackendResult<TableSchema> {
    let name = schema.relation.qualified_name();
    let mut previous = fence_data(read, batch, &name)?;
    if previous.is_none() {
        if let Some(bytes) = read.get(&relation_key(TAG_TABLE, &schema.relation)?)? {
            let existing: TableSchema = decode_value(&bytes)?;
            if existing.relation != schema.relation {
                return Err(other_error(
                    "table definition disagrees with its catalog key",
                ));
            }
            if existing.object_id != [0; 16] && existing.storage_generation != [0; 16] {
                previous = Some(Owner {
                    object: existing.object_id,
                    generation: existing.storage_generation,
                    catalog: true,
                });
            }
        }
    }
    let owner = Owner::new(
        if schema.object_id == [0; 16] {
            previous.map_or([0; 16], |owner| owner.object)
        } else {
            schema.object_id
        },
        if schema.storage_generation == [0; 16] {
            previous.map_or([0; 16], |owner| owner.generation)
        } else {
            schema.storage_generation
        },
        true,
    )?;
    if let Some(previous) = previous {
        batch.fence_record(&previous.marker())?;
        if (owner.object, owner.generation) != (previous.object, previous.generation) {
            seed(read, batch, &name, previous)?;
        }
        if owner.object != previous.object {
            owner.inherit(batch, previous)?;
            batch.delete(&previous.identity_key())?;
        }
    }
    if previous.is_some_and(|previous| {
        previous.object == owner.object && previous.generation != owner.generation
    }) {
        seed_generation(read, batch, &name, owner)?;
    } else {
        seed(read, batch, &name, owner)?;
    }
    batch.fence_record(&owner.marker())?;
    claim(read, batch, &name, owner, None)?;
    let mut schema = schema.clone();
    schema.object_id = owner.object;
    schema.storage_generation = owner.generation;
    Ok(schema)
}

pub(in crate::key_value) fn drop_binding(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    data: bool,
) -> StorageBackendResult<()> {
    let mut owner = fence_data(read, batch, name)?;
    if !data && owner.is_none() {
        if let Some(key) = super::catalog_key(name)? {
            if read.get(&key)?.is_some() {
                owner = Some(document_owner(read, batch, name)?);
            }
        }
    }
    if let Some(mut owner) = owner {
        if data {
            retire(batch, name, owner, read)?;
        } else if owner.catalog {
            owner.catalog = false;
            batch.put(&binding_key(name, read.control())?, &owner.encode())?;
        }
    }
    Ok(())
}

fn source_owner(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    has_data: &mut impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Option<Owner>> {
    let owner = match lookup(read, name)? {
        Some(owner) => Some(owner),
        None if has_data(name)?
            || read
                .get(&super::single_str_key(
                    super::TAG_METADATA,
                    &super::legacy_document_id_metadata_key(name),
                )?)?
                .is_some_and(|value| !value.is_empty()) =>
        {
            Some(document_owner(read, batch, name)?)
        }
        None => None,
    };
    batch.fence_record(&binding_key(name, read.control())?)?;
    if let Some(owner) = owner {
        batch.fence_record(&owner.marker())?;
    }
    Ok(owner)
}

pub(in crate::key_value) fn rename_bindings(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    from: &str,
    to: &str,
    schema: &TableSchema,
    mut has_data: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<TableSchema> {
    let mut schema = prepare_table(read, batch, schema)?;
    let old_name = schema.relation.qualified_name();
    let relation = RelationIdentity::from_legacy_name(to).map_err(other_error)?;
    let new_name = relation.qualified_name();
    let owner = Owner {
        object: schema.object_id,
        generation: schema.storage_generation,
        catalog: true,
    };
    let source = if from == old_name {
        Some(owner)
    } else {
        source_owner(read, batch, from, &mut has_data)?
    };
    let canonical_target = source_owner(read, batch, &new_name, &mut has_data)?;
    batch.delete(&binding_key(&old_name, read.control())?)?;

    if from != old_name && has_data(&old_name)? {
        let retained = Owner::new([0; 16], [0; 16], false)?;
        retained.inherit(batch, owner)?;
        seed(read, batch, &old_name, retained)?;
        claim(read, batch, &old_name, retained, None)?;
    }

    if let Some(source) = source {
        let target = if to == new_name {
            owner
        } else if let Some(target) = source_owner(read, batch, to, &mut has_data)? {
            target
        } else {
            Owner::new([0; 16], [0; 16], false)?
        };
        target.inherit(batch, source)?;
        if to != new_name {
            seed(read, batch, to, target)?;
            claim(read, batch, to, target, None)?;
        }
        if from != old_name {
            retire(batch, from, source, read)?;
        }
    }
    if let Some(previous) = canonical_target {
        owner.inherit(batch, previous)?;
        if previous.object != owner.object {
            batch.delete(&previous.identity_key())?;
        }
    }
    seed(read, batch, &new_name, owner)?;
    batch.fence_record(&owner.marker())?;
    claim(read, batch, &new_name, owner, Some(&old_name))?;
    schema.relation = relation;
    Ok(schema)
}
