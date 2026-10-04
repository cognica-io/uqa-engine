//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode one committed IVF generation while retaining every payload allowance.

use crate::mvcc::{
    CommittedRecordSnapshot, IVFRecordHeader, IVFRecordKey as Key, IVFRecordLayout, VersionResult,
};
use crate::{
    ivf_index::{IVFMetadataSnapshot, IVFPreparedMetadata, IVFRestoreBuilder},
    read_control::StorageReadControl,
};

pub(super) fn index(
    key: &[u8],
    header: IVFRecordHeader,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<IVFPreparedMetadata> {
    let mut builder = IVFRestoreBuilder::new(
        header.dimensions,
        header.params,
        IVFMetadataSnapshot {
            state: header.state,
            centroids: Vec::new(),
            assignments: Vec::new(),
            trained_size: header.trained_size,
            deletes_since_train: header.deletes_since_train,
            vector_count: header.vector_count,
        },
        control,
    )?;
    current.visit_prefix(
        &layout.key(key, Key::Centroids, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (id, vector) = layout.centroid(key, value, control)?;
                builder.centroid(id, &vector)?;
            }
            Ok(true)
        },
    )?;
    current.visit_prefix(
        &layout.key(key, Key::Assignments, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (document, ordinal, centroid) = layout.assignment(key, value, control)?;
                builder.assignment(document, ordinal, centroid)?;
            }
            Ok(true)
        },
    )?;
    current.visit_prefix(
        &layout.key(key, Key::Vectors, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (document, ordinal, vector) = layout.vector(key, value, control)?;
                builder.vector(document, ordinal, &vector)?;
            }
            Ok(true)
        },
    )?;
    Ok(builder.finish()?)
}
