//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native B-tree definitions, repairs and evaluated per-document postings share the document transaction.

pub(super) mod columns;

use rusqlite::types::{FromSql, ValueRef};
use uqa_storage::{KeyValueBatch, ValueIndexKey};

use super::{
    decode_doc_id, decode_value, encode_doc_id, encode_value, BTreeMap, DocId, Result, SQLiteError,
    SQLiteValueIndexKey, Value,
};
use crate::mvcc::native::{
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot,
};

fn field_value(field: ValueRef<'_>) -> Result<ValueIndexKey> {
    SQLiteValueIndexKey::column_result(field)
        .map(|key| key.0)
        .map_err(|error| {
            SQLiteError::StorageBackend(format!("invalid native B-tree field: {error}"))
        })
}

pub(super) fn fields(snapshot: &NativeSnapshot, table: &str) -> Result<Vec<ValueIndexKey>> {
    let mut fields = Vec::new();
    if let Some(owner) = snapshot.table_owner(table)? {
        snapshot.visit_rows(Family::BtreeIndexes, Some(owner), &[], |row| {
            fields.push(field_value(row[1])?);
            Ok(())
        })?;
    }
    Ok(fields)
}

pub(super) fn repairs(snapshot: &NativeSnapshot) -> Result<Vec<(String, ValueIndexKey)>> {
    let mut repairs = Vec::new();
    snapshot.visit_rows(Family::BtreeIndexRepairs, None, &[], |row| {
        let table = row[0].as_str().map_err(|_| {
            SQLiteError::StorageBackend("native B-tree repair table must be text".into())
        })?;
        repairs.push((table.to_owned(), field_value(row[1])?));
        Ok(())
    })?;
    // Record keys use stable owners, while the public result is sorted by current table names.
    repairs.sort_unstable();
    Ok(repairs)
}

pub(super) fn clear_repair(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &ValueIndexKey,
) -> Result<()> {
    if let Some(owner) = snapshot.table_owner(table)? {
        snapshot.delete_prefix(
            batch,
            Family::BtreeIndexRepairs,
            owner,
            &[SQLiteValueIndexKey(field).as_value_ref()],
        )?;
    }
    Ok(())
}

pub(super) fn load(
    snapshot: &NativeSnapshot,
    table: &str,
    field: &ValueIndexKey,
) -> Result<Option<Vec<(DocId, Value)>>> {
    let Some(owner) = snapshot.table_owner(table)? else {
        return Ok(None);
    };
    let field = SQLiteValueIndexKey(field);
    if !snapshot.contains_row(Family::BtreeIndexes, owner, &[field.as_value_ref()])? {
        return Ok(None);
    }
    let mut values = Vec::new();
    snapshot.visit_rows(
        Family::BtreeIndexEntries,
        Some(owner),
        &[field.as_value_ref()],
        |row| {
            let doc_id = row[2].as_i64().map_err(|_| {
                SQLiteError::StorageBackend("native B-tree document id must be integer".into())
            })?;
            let value = row[3].as_str().map_err(|_| {
                SQLiteError::StorageBackend("native B-tree value must be text".into())
            })?;
            values.push((decode_doc_id(doc_id)?, decode_value(value)?));
            Ok(())
        },
    )?;
    Ok(Some(values))
}

fn mark(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    owner: NativeRecordOwner,
    field: ValueRef<'_>,
) -> Result<()> {
    // Existing definitions are immutable for a row update: independent documents must not acquire a shared write precondition on the marker.
    if !snapshot.contains_row(Family::BtreeIndexes, owner, &[field])? {
        snapshot.put_row(
            batch,
            Family::BtreeIndexes,
            owner,
            &[ValueRef::Text(table.as_bytes()), field],
        )?;
    }
    Ok(())
}

fn entry(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    owner: NativeRecordOwner,
    field: ValueRef<'_>,
    doc_id: DocId,
    value: &Value,
) -> Result<()> {
    let id = encode_doc_id(doc_id)?;
    if !snapshot.contains_row(Family::Documents, owner, &[ValueRef::Integer(id)])? {
        return Err(SQLiteError::StorageBackend(
            "persistent B-tree entry has no backing document".into(),
        ));
    }
    let encoded = encode_value(value)?;
    snapshot.put_row(
        batch,
        Family::BtreeIndexEntries,
        owner,
        &[
            ValueRef::Text(table.as_bytes()),
            field,
            ValueRef::Integer(id),
            ValueRef::Text(encoded.as_bytes()),
        ],
    )
}

pub(super) fn replace_many(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    indexes: &[(&ValueIndexKey, &[(DocId, Value)])],
) -> Result<()> {
    if indexes.is_empty() {
        return Ok(());
    }
    let owner = snapshot.ensure_table_owner(table, batch)?;
    for (field, values) in indexes {
        let field = SQLiteValueIndexKey(*field);
        mark(snapshot, batch, table, owner, field.as_value_ref())?;
        snapshot.delete_prefix(
            batch,
            Family::BtreeIndexEntries,
            owner,
            &[field.as_value_ref()],
        )?;
        let mut ids = uqa_core::memory::BudgetedVec::new(snapshot.control.memory());
        for (id, _) in *values {
            ids.push(encode_doc_id(*id)?)?;
        }
        ids.sort_unstable();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(SQLiteError::StorageBackend(
                "duplicate document id in persisted B-tree replacement".into(),
            ));
        }
        for (id, value) in *values {
            entry(
                snapshot,
                batch,
                table,
                owner,
                field.as_value_ref(),
                *id,
                value,
            )?;
        }
    }
    Ok(())
}

pub(super) fn repair(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &ValueIndexKey,
    stale: &[DocId],
    missing: &[(DocId, Value)],
) -> Result<()> {
    let owner = snapshot.ensure_table_owner(table, batch)?;
    let field = SQLiteValueIndexKey(field);
    mark(snapshot, batch, table, owner, field.as_value_ref())?;
    for &id in stale {
        snapshot.delete_prefix(
            batch,
            Family::BtreeIndexEntries,
            owner,
            &[field.as_value_ref(), ValueRef::Integer(encode_doc_id(id)?)],
        )?;
    }
    for (id, value) in missing {
        entry(
            snapshot,
            batch,
            table,
            owner,
            field.as_value_ref(),
            *id,
            value,
        )?;
    }
    Ok(())
}

pub(super) fn apply_write(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    doc_id: DocId,
    values: Option<&BTreeMap<ValueIndexKey, Value>>,
) -> Result<()> {
    let id = encode_doc_id(doc_id)?;
    let Some(owner) = snapshot.table_owner(table)? else {
        return Ok(());
    };
    if let Some(values) = values {
        for (field, value) in values {
            let field = SQLiteValueIndexKey(field);
            if snapshot.contains_row(Family::BtreeIndexes, owner, &[field.as_value_ref()])? {
                entry(
                    snapshot,
                    batch,
                    table,
                    owner,
                    field.as_value_ref(),
                    doc_id,
                    value,
                )?;
            }
        }
    } else {
        delete_document(snapshot, batch, owner, id)?;
    }
    Ok(())
}

pub(super) fn drop_index(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &ValueIndexKey,
) -> Result<()> {
    if let Some(owner) = snapshot.table_owner(table)? {
        for family in [Family::BtreeIndexEntries, Family::BtreeIndexes] {
            snapshot.delete_prefix(
                batch,
                family,
                owner,
                &[SQLiteValueIndexKey(field).as_value_ref()],
            )?;
        }
    }
    Ok(())
}

pub(super) fn clear_table(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
) -> Result<()> {
    if let Some(owner) = snapshot.table_owner(table)? {
        snapshot.delete_prefix(batch, Family::BtreeIndexEntries, owner, &[])?;
    }
    Ok(())
}

pub(crate) fn delete_document(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    owner: NativeRecordOwner,
    id: i64,
) -> Result<()> {
    let identity = NativeRecordIdentity::new(Family::BtreeIndexEntries, owner)?;
    let control = &snapshot.control;
    let prefix = identity.encode_prefix(&[], control)?;
    let mut after = None;
    loop {
        let mut next = None;
        // Seek once per physical field namespace, skipping its remaining document keys and historical tombstones.
        snapshot
            .view
            .visit_keys(&prefix, after.as_deref(), 1, control, &mut |key, _| {
                NativeRecordIdentity::visit_key_components(key, control, |component, field| {
                    if component == 0 {
                        batch.delete(
                            &identity.encode_key(&[field, ValueRef::Integer(id)], control)?,
                        )?;
                        next = Some(
                            identity.encode_key(&[field, ValueRef::Integer(i64::MAX)], control)?,
                        );
                    }
                    Ok(())
                })?;
                Ok(false)
            })?;
        let Some(next) = next else { break };
        after = Some(next);
    }
    Ok(())
}
