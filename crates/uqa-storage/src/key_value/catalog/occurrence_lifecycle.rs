//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column ownership for binary occurrence keys and original-source metadata.

use super::super::occurrence_keys as keys;
use super::{
    batch_put_or_keep_existing, batch_rekey_prefix_or_keep_existing, KeyValueBatch, KeyValueStore,
    StorageBackendResult,
};

pub(super) fn drop_occurrence_field(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
) -> StorageBackendResult<()> {
    for kind in [keys::SCORE, keys::POSITIONS, keys::METADATA, keys::FIELD] {
        batch.delete_prefix(&keys::field_prefix(table, kind, field)?)?;
    }
    for kind in [keys::LENGTH, keys::DOCUMENT] {
        for (key, _) in store.scan_prefix(&keys::kind_prefix(table, kind)?)? {
            if keys::read_document(&key, kind)?.1 == field {
                batch.delete(&key)?;
            }
        }
    }
    Ok(())
}

pub(super) fn rename_occurrence_field(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    if from == to {
        return Ok(());
    }
    for kind in [keys::SCORE, keys::POSITIONS, keys::METADATA, keys::FIELD] {
        batch_rekey_prefix_or_keep_existing(
            store,
            batch,
            &keys::field_prefix(table, kind, from)?,
            &keys::field_prefix(table, kind, to)?,
        )?;
    }
    for kind in [keys::LENGTH, keys::DOCUMENT] {
        for (key, value) in store.scan_prefix(&keys::kind_prefix(table, kind)?)? {
            let (doc_id, field) = keys::read_document(&key, kind)?;
            if field == from {
                batch_put_or_keep_existing(
                    store,
                    batch,
                    &keys::document_key(table, kind, doc_id, to)?,
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
    }
    Ok(())
}
