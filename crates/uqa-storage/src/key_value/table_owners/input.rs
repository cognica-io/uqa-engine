//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A decoded original table and its retained view share the existing binding predicates.

use super::{binding_key, other_error, Owner};
use crate::{
    catalog::input::CatalogInputLimits, key_value::KeyValueRead, read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult, TableSchema,
};
use uqa_core::memory::ProductionControl;

#[cfg(test)]
mod tests;

pub(in crate::key_value) fn validate_table_with_control(
    read: &dyn KeyValueRead,
    schema: &TableSchema,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    read.control().check()?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let name = schema.relation.qualified_name_with_control(&production)?;
    let key = binding_key(name.as_str(), control)?;
    control.check_value_size(key.len(), limits.maximum_key_bytes)?;
    let mut owner = None;
    read.visit_value_bounded(&key, limits.maximum_value_bytes, control, &mut |value| {
        owner = value.map(Owner::decode).transpose()?;
        Ok(())
    })?;
    if let Some(owner) = owner {
        control.check_value_size(owner.identity_key().len(), limits.maximum_key_bytes)?;
        let mut matches = false;
        read.visit_value_bounded(
            &owner.identity_key(),
            limits.maximum_value_bytes,
            control,
            &mut |value| {
                matches = value == Some(name.as_str().as_bytes());
                Ok(())
            },
        )?;
        if !matches {
            return Err(other_error(
                "table owner identity index disagrees with its binding",
            ));
        }
        // The definition already decoded on this exact view is the catalog lookup result. Invalid canonical references keep the original absent-definition behavior.
        let present = RelationIdentity::borrowed_reference(name.as_str()).is_some();
        if owner.catalog != present {
            return Err(other_error(
                "table owner binding disagrees with its catalog definition",
            ));
        }
        if present
            && (schema.object_id != owner.object || schema.storage_generation != owner.generation)
        {
            return Err(other_error(
                "table definition disagrees with its owner identities",
            ));
        }
    }
    control.check()?;
    read.control().check()
}
