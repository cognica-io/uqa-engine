//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode native private IVF previews and retain immutable document inputs in the same batch.

use super::super::metadata::{state_to_str, EncodedIVFMetadata};
use crate::mvcc::native::{
    NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
};
use crate::vector_index::native::NativeVectorRead;
use crate::{Result, SQLiteError};
use rusqlite::types::ValueRef;
use uqa_storage::{ivf_index::IVFMutation, KeyValueBatch};

pub(super) fn write_input(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &EncodedIVFMetadata,
    mutation: IVFMutation<'_>,
) -> Result<()> {
    let preview = super::load_metadata(read)?.is_some();
    if preview {
        let key = super::records::metadata_key(
            read.owner.expect("owned native mutation"),
            &read.index.field,
            &read.snapshot.control,
        )?;
        batch.ivf_mutation(&key, mutation)?;
    }
    write_metadata(read, batch, metadata, preview)
}

fn put_row(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    preview: bool,
    family: Family,
    owner: NativeRecordOwner,
    values: &[ValueRef<'_>],
) -> Result<()> {
    let record = NativeRecord::encode(family, owner, values, &read.snapshot.control)?;
    if preview {
        batch.preview_ivf_record(record.key(), Some(record.row()))?;
    } else {
        batch.put(record.key(), record.row())?;
    }
    Ok(())
}

pub(in crate::vector_index::ivf) fn write_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &EncodedIVFMetadata,
    preview: bool,
) -> Result<()> {
    let owner = read.owner.ok_or_else(|| {
        SQLiteError::StorageBackend("IVF publication requires a native table owner".into())
    })?;
    if !preview {
        read.snapshot
            .fence_ivf_definitions(batch, owner, Some(&read.index.field))?;
    }
    let table = ValueRef::Text(read.index.table.as_bytes());
    put_row(
        read,
        batch,
        preview,
        Family::IVFIndexes,
        owner,
        &[
            table,
            read.field(),
            ValueRef::Integer(i64::from(read.index.dimensions)),
            ValueRef::Integer(metadata.nlist),
            ValueRef::Integer(metadata.nprobe),
            ValueRef::Integer(metadata.train_threshold),
            ValueRef::Text(state_to_str(metadata.state).as_bytes()),
            ValueRef::Integer(metadata.trained_size),
            ValueRef::Integer(metadata.deletes_since_train),
            ValueRef::Integer(metadata.vector_count),
        ],
    )?;
    for family in [Family::IVFCentroids, Family::IVFAssignments] {
        let prefix = NativeRecordIdentity::new(family, owner)?
            .encode_prefix(&[read.field()], &read.snapshot.control)?;
        if preview {
            batch.preview_ivf_prefix(&prefix)?;
        } else {
            batch.delete_prefix(&prefix)?;
        }
    }
    for (centroid, vector) in &metadata.centroids {
        put_row(
            read,
            batch,
            preview,
            Family::IVFCentroids,
            owner,
            &[
                table,
                read.field(),
                ValueRef::Integer(*centroid),
                ValueRef::Blob(vector),
            ],
        )?;
    }
    for (doc, ordinal, centroid) in &metadata.assignments {
        put_row(
            read,
            batch,
            preview,
            Family::IVFAssignments,
            owner,
            &[
                table,
                read.field(),
                ValueRef::Integer(*doc),
                ValueRef::Integer(*ordinal),
                ValueRef::Integer(*centroid),
            ],
        )?;
    }
    Ok(())
}

pub(in crate::vector_index::ivf) fn drop_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
) -> Result<()> {
    if let Some(owner) = read.owner {
        read.snapshot
            .fence_ivf_definitions(batch, owner, Some(&read.index.field))?;
    }
    for family in [
        Family::IVFAssignments,
        Family::IVFCentroids,
        Family::IVFIndexes,
    ] {
        read.clear_family(batch, family)?;
    }
    Ok(())
}
