//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode native private IVF previews and retain immutable document inputs in the same batch.

use super::super::metadata::{state_to_str, EncodedIVFMetadata};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::{publication::VectorPublication, NativeVectorRead};
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
            .fence_vector_definitions(batch, owner, Some(&read.index.field))?;
    }
    let publication = if preview {
        VectorPublication::IVFPreview
    } else {
        VectorPublication::Canonical
    };
    let table = ValueRef::Text(read.index.table.as_bytes());
    publication.put_row(
        read,
        batch,
        Family::IVFIndexes,
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
        publication.delete_prefix(read, batch, family, &[read.field()])?;
    }
    for (centroid, vector) in &metadata.centroids {
        publication.put_row(
            read,
            batch,
            Family::IVFCentroids,
            &[
                table,
                read.field(),
                ValueRef::Integer(*centroid),
                ValueRef::Blob(vector),
            ],
        )?;
    }
    for (doc, ordinal, centroid) in &metadata.assignments {
        publication.put_row(
            read,
            batch,
            Family::IVFAssignments,
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
            .fence_vector_definitions(batch, owner, Some(&read.index.field))?;
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
