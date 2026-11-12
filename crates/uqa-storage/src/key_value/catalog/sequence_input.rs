//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence definitions and their generation values share one selected catalog view.

mod authority;
mod row;

use super::{KeyValueCatalog, TAG_SEQUENCE, TAG_SEQUENCE_VALUE};
use crate::{
    catalog::{
        input::{strings::take, CatalogInputLimits},
        sequence_load::{CatalogSequenceDefinition, CatalogSequenceLoad},
        SequenceValuePosition,
    },
    key_value::KeyValueRead,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use uqa_core::memory::Budgeted;

impl KeyValueCatalog {
    pub(super) fn sequence_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogSequenceLoad> {
        control.check()?;
        limits.validate()?;
        if !self.store.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "retained sequence input requires a versioned read owner".into(),
            ));
        }
        let read = crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            read.retain(&[&[TAG_SEQUENCE], &[TAG_SEQUENCE_VALUE]])
        })?;
        CatalogSequenceLoad::new(read, &[TAG_SEQUENCE], decode, position, limits, control)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogSequenceDefinition>>> {
    let relation = super::keys::decode_relation_key_with_control(key, control)?;
    let stored = row::read(value, control)?;
    let legacy_position = stored.legacy_value();
    let (stored, mut memory) = stored.into_parts();
    Ok(Some(Budgeted::new(
        CatalogSequenceDefinition {
            row: crate::SequenceRow {
                relation: take(relation, &mut memory),
                security: stored.security,
                object_id: stored.object_id,
                definition_generation: stored.definition_generation,
                start: stored.start,
                increment: stored.increment,
                current: 0,
                called: false,
                log_count: 0,
                persistence: stored.persistence,
                owner: stored.owner,
                options: stored.options,
            },
            legacy_position,
        },
        memory,
    )))
}

fn position(
    read: &dyn KeyValueRead,
    definition: &CatalogSequenceDefinition,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<SequenceValuePosition> {
    let row = &definition.row;
    let mut key = [TAG_SEQUENCE_VALUE; 33];
    key[1..17].copy_from_slice(&row.object_id);
    key[17..].copy_from_slice(&row.definition_generation);
    control.check_value_size(key.len(), limits.maximum_key_bytes)?;
    let mut position = None;
    read.visit_value_bounded(&key, limits.maximum_value_bytes, control, &mut |bytes| {
        position = bytes
            .map(|bytes| row::value(bytes, control).map(|value| value.position()))
            .transpose()?;
        Ok(())
    })?;
    control.check()?;
    read.control().check()?;
    super::sequences::resolve_position(&row.relation, position, definition.legacy_position)
}

#[cfg(test)]
mod tests;
