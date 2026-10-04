//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row decoding and encoding for common IVF candidate preparation.

use uqa_storage::{
    ivf_index::{IVFMetadataSnapshot, IVFState},
    IVFIndexParams,
};

use super::load_metadata;
use crate::{
    mvcc::native::NativeRecordFamily as Family,
    vector_index::{
        decode_doc_id,
        native::{integer, NativeVectorRead},
    },
    Result, SQLiteError,
};

pub(in crate::vector_index::ivf) fn load_state(
    read: &NativeVectorRead<'_>,
    params: IVFIndexParams,
    rebuild: bool,
) -> Result<uqa_storage::ivf_index::IVFPreparedMetadata> {
    let count = read.count()?;
    let mut header = IVFMetadataSnapshot {
        state: IVFState::Untrained,
        centroids: Vec::new(),
        assignments: Vec::new(),
        trained_size: 0,
        deletes_since_train: 0,
        vector_count: count,
    };
    if !rebuild {
        if let Some(meta) = load_metadata(read)? {
            if meta.dimensions != read.index.dimensions || meta.params != params {
                return Err(SQLiteError::StorageBackend(
                    "native IVF definition does not match its handle".into(),
                ));
            }
            header.state = meta.state;
            header.trained_size = meta.trained_size;
            header.deletes_since_train = meta.deletes_since_train;
            header.vector_count = meta.vector_count;
        } else if count > 0 {
            return Err(SQLiteError::StorageBackend(
                "missing native IVF metadata".into(),
            ));
        }
    }
    let mut builder = uqa_storage::ivf_index::IVFRestoreBuilder::new(
        read.index.dimensions,
        params,
        header,
        &read.snapshot.control,
    )?;
    if let Some(owner) = read.owner {
        if !rebuild {
            read.snapshot.visit_rows(
                Family::IVFCentroids,
                Some(owner),
                &[read.field()],
                |row| {
                    let bytes = super::blob(row[3])?;
                    let _memory = read.snapshot.control.memory().reserve(bytes.len())?;
                    let vector = crate::vector_index::blob_to_vector(bytes)?;
                    read.index.validate_dimensions_sqlite(&vector)?;
                    builder.centroid(
                        usize::try_from(integer(row[2])?).map_err(|_| {
                            SQLiteError::StorageBackend(
                                "invalid native IVF centroid sequence".into(),
                            )
                        })?,
                        &vector,
                    )?;
                    Ok(())
                },
            )?;
            read.snapshot.visit_rows(
                Family::IVFAssignments,
                Some(owner),
                &[read.field()],
                |row| {
                    builder.assignment(
                        decode_doc_id(integer(row[2])?)?,
                        u32::try_from(integer(row[3])?).map_err(|_| {
                            SQLiteError::StorageBackend("invalid native IVF ordinal".into())
                        })?,
                        usize::try_from(integer(row[4])?).map_err(|_| {
                            SQLiteError::StorageBackend("invalid native IVF centroid".into())
                        })?,
                    )?;
                    Ok(())
                },
            )?;
        }
    }
    read.visit_ordered_vectors(|document, ordinal, vector| {
        builder.vector(document, ordinal, vector)?;
        Ok(())
    })?;
    Ok(builder.finish()?)
}
