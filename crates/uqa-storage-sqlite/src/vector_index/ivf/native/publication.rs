//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode native private IVF previews and retain immutable document inputs in the same batch.

use super::super::metadata::{state_to_str, usize_to_i64, EncodedIVFMetadata};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::{publication::VectorPublication, NativeVectorRead};
use crate::{Result, SQLiteError};
use rusqlite::types::ValueRef;
use uqa_storage::{
    ivf_index::{IVFMutation, IVFPreparedMetadata},
    KeyValueBatch,
};

pub(super) fn write_input(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &IVFPreparedMetadata,
    mutation: IVFMutation<'_>,
    full_rewrite: bool,
) -> Result<()> {
    let preview = super::load_metadata(read)?.is_some();
    if preview {
        let key = super::records::metadata_key(
            read.owner.expect("owned native mutation"),
            &read.index.field,
            &read.snapshot.control,
        )?;
        batch.ivf_mutation(&key, mutation)?;
        if !full_rewrite {
            if let IVFMutation::Replace { document, .. } | IVFMutation::Delete(document) = mutation
            {
                return write_document(read, batch, metadata, document);
            }
        }
    }
    write_prepared(read, batch, metadata, preview)
}

pub(in crate::vector_index::ivf) fn write_prepared(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &IVFPreparedMetadata,
    preview: bool,
) -> Result<()> {
    let encoded = encode_header(metadata)?;
    let header = metadata.header();
    write_metadata(read, batch, &encoded, preview)?;
    let publication = if preview {
        VectorPublication::IVFPreview
    } else {
        VectorPublication::Canonical
    };
    let table = ValueRef::Text(read.index.table.as_bytes());
    for (id, vector) in header.centroids.iter().enumerate() {
        read.snapshot.control.check()?;
        let _memory = read.snapshot.control.memory().reserve(
            vector
                .len()
                .checked_mul(4)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        let bytes = crate::vector_index::vector_to_blob(vector)?;
        publication.put_row(
            read,
            batch,
            Family::IVFCentroids,
            &[
                table,
                read.field(),
                ValueRef::Integer(usize_to_i64("centroid_id", id)?),
                ValueRef::Blob(&bytes),
            ],
        )?;
    }
    for entry in metadata.assignments() {
        let (doc, ordinal, centroid) = entry?;
        put_assignment(read, batch, publication, doc, ordinal, centroid)?;
    }
    Ok(())
}

fn encode_header(metadata: &IVFPreparedMetadata) -> Result<EncodedIVFMetadata> {
    let header = metadata.header();
    Ok(EncodedIVFMetadata {
        nlist: usize_to_i64("nlist", metadata.params().nlist)?,
        nprobe: usize_to_i64("nprobe", metadata.params().nprobe)?,
        train_threshold: usize_to_i64("train_threshold", metadata.params().train_threshold)?,
        state: header.state,
        trained_size: usize_to_i64("trained_size", header.trained_size)?,
        deletes_since_train: usize_to_i64("deletes_since_train", header.deletes_since_train)?,
        vector_count: usize_to_i64("vector_count", header.vector_count)?,
        centroids: Vec::new(),
        assignments: Vec::new(),
    })
}

fn write_document(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &IVFPreparedMetadata,
    document: uqa_core::DocId,
) -> Result<()> {
    let publication = VectorPublication::IVFPreview;
    write_header(read, batch, &encode_header(metadata)?, true)?;
    publication.delete_prefix(
        read,
        batch,
        Family::IVFAssignments,
        &[
            read.field(),
            ValueRef::Integer(crate::vector_index::encode_doc_id(document)?),
        ],
    )?;
    for assignment in metadata.document_assignments(document) {
        let (ordinal, centroid) = assignment?;
        put_assignment(read, batch, publication, document, ordinal, centroid)?;
    }
    Ok(())
}

fn put_assignment(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    publication: VectorPublication,
    document: uqa_core::DocId,
    ordinal: u32,
    centroid: usize,
) -> Result<()> {
    #[cfg(test)]
    ASSIGNMENT_WRITES.set(ASSIGNMENT_WRITES.get() + 1);
    publication.put_row(
        read,
        batch,
        Family::IVFAssignments,
        &[
            ValueRef::Text(read.index.table.as_bytes()),
            read.field(),
            ValueRef::Integer(crate::vector_index::encode_doc_id(document)?),
            ValueRef::Integer(i64::from(ordinal)),
            ValueRef::Integer(super::super::metadata::usize_to_i64(
                "centroid_id",
                centroid,
            )?),
        ],
    )
}

#[cfg(test)]
thread_local! {
    pub(super) static ASSIGNMENT_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::vector_index::ivf) fn write_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &EncodedIVFMetadata,
    preview: bool,
) -> Result<()> {
    write_header(read, batch, metadata, preview)?;
    let publication = if preview {
        VectorPublication::IVFPreview
    } else {
        VectorPublication::Canonical
    };
    let table = ValueRef::Text(read.index.table.as_bytes());
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

fn write_header(
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
    )
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
