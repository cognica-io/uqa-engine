//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF canonical values and persisted metadata share one native read and mutation boundary.

use rusqlite::types::ValueRef;
use uqa_core::{memory::Budgeted, DocId, PostingList};
use uqa_storage::{ivf_index::IVFState, KeyValueBatch};

use super::{
    math::{nearest_centroids, scored_posting_list},
    metadata::{decode_metadata, encode_metadata, state_to_str, EncodedIVFMetadata, SQLiteIVFMeta},
    SQLiteIVFIndex,
};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::{
    blob_to_vector, decode_doc_id,
    native::{blob, integer, text, NativeVectorRead, VectorBuffer},
};
use crate::{Result, SQLiteError};

type Candidates = Vec<(DocId, Vec<f32>)>;

pub(super) fn load_metadata(read: &NativeVectorRead<'_>) -> Result<Option<SQLiteIVFMeta>> {
    let Some(owner) = read.owner else {
        return Ok(None);
    };
    read.snapshot
        .read_row(Family::IVFIndexes, owner, &[read.field()], |row| {
            decode_metadata((
                integer(row[2])?,
                integer(row[3])?,
                integer(row[4])?,
                integer(row[5])?,
                text(row[6])?.to_owned(),
                integer(row[7])?,
                integer(row[8])?,
                integer(row[9])?,
            ))
        })
}

pub(super) fn write_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    metadata: &EncodedIVFMetadata,
) -> Result<()> {
    let owner = read.owner.ok_or_else(|| {
        SQLiteError::StorageBackend("IVF publication requires a native table owner".into())
    })?;
    let table = ValueRef::Text(read.index.table.as_bytes());
    read.snapshot.put_row(
        batch,
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
    read.clear_family(batch, Family::IVFCentroids)?;
    read.clear_family(batch, Family::IVFAssignments)?;
    for (centroid, vector) in &metadata.centroids {
        read.snapshot.put_row(
            batch,
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
        read.snapshot.put_row(
            batch,
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

pub(super) fn drop_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
) -> Result<()> {
    for family in [
        Family::IVFAssignments,
        Family::IVFCentroids,
        Family::IVFIndexes,
    ] {
        read.clear_family(batch, family)?;
    }
    Ok(())
}

impl SQLiteIVFIndex {
    pub(super) fn replace_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        doc: i64,
        encoded: &[(i64, Vec<u8>)],
        vectors: &[Vec<f32>],
    ) -> Result<()> {
        let read = read.owned(batch)?;
        // Tuple fields drop in order, releasing vector allocations before their allowance.
        let mut prospective = read.vectors()?.into_parts();
        let (entries, retained) = (&mut prospective.0, &mut prospective.1);
        let doc_id = decode_doc_id(doc)?;
        entries.retain(|(id, _, _)| *id != doc_id);
        for (ordinal, vector) in vectors.iter().enumerate() {
            let bytes = std::mem::size_of::<(DocId, u32, Vec<f32>)>()
                .checked_add(
                    vector
                        .len()
                        .checked_mul(4)
                        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
                )
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            retained.grow(bytes)?;
            entries.try_reserve_exact(1).map_err(|error| {
                SQLiteError::StorageBackend(format!("native IVF vector allocation failed: {error}"))
            })?;
            entries.push((
                doc_id,
                u32::try_from(ordinal).map_err(|_| {
                    SQLiteError::StorageBackend("invalid native vector ordinal".into())
                })?,
                vector.clone(),
            ));
        }
        entries.sort_by_key(|(doc, ordinal, _)| (*doc, *ordinal));
        let metadata = encode_metadata(self.params, &self.metadata_for_entries(entries)?)?;
        read.replace(batch, doc, encoded)?;
        write_metadata(&read, batch, &metadata)
    }

    pub(super) fn delete_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        doc: i64,
    ) -> Result<()> {
        let mut prospective = read.vectors()?.into_parts();
        let entries = &mut prospective.0;
        let count = entries.len();
        let doc_id = decode_doc_id(doc)?;
        entries.retain(|(id, _, _)| *id != doc_id);
        if entries.len() == count {
            return Ok(());
        }
        let metadata = encode_metadata(self.params, &self.metadata_for_entries(entries)?)?;
        read.delete(batch, doc)?;
        write_metadata(read, batch, &metadata)
    }

    pub(super) fn search_native(
        &self,
        read: &NativeVectorRead<'_>,
        query: &[f32],
        k: usize,
    ) -> Result<PostingList> {
        // The metadata and canonical count must be observed at this same retained boundary.
        let matching_count = match load_metadata(read)? {
            Some(meta)
                if meta.state == IVFState::Trained
                    && meta.dimensions == self.persistent.dimensions
                    && meta.params == self.params =>
            {
                meta.vector_count == read.count()?
            }
            _ => false,
        };
        if !matching_count {
            return exact(read, query, k);
        }
        let centroids = load_centroids(read)?;
        if centroids.is_empty() {
            return exact(read, query, k);
        }
        let probes = nearest_centroids(query, &centroids, self.params.nprobe);
        let candidates = load_candidates(read, &probes)?;
        Ok(scored_posting_list(
            query,
            candidates
                .iter()
                .map(|(doc, vector)| (*doc, vector.as_slice())),
            k,
        ))
    }
}

fn exact(read: &NativeVectorRead<'_>, query: &[f32], k: usize) -> Result<PostingList> {
    let entries = read.vectors()?;
    Ok(scored_posting_list(
        query,
        entries
            .iter()
            .map(|(doc, _, vector)| (*doc, vector.as_slice())),
        k,
    ))
}

fn load_centroids(read: &NativeVectorRead<'_>) -> Result<Budgeted<Vec<Vec<f32>>>> {
    let mut output = VectorBuffer::new(read)?;
    let (centroids, payload) = (&mut output.rows, &mut output.payload);
    if let Some(owner) = read.owner {
        read.snapshot
            .visit_rows(Family::IVFCentroids, Some(owner), &[read.field()], |row| {
                if usize::try_from(integer(row[2])?).ok() != Some(centroids.len()) {
                    return Err(SQLiteError::StorageBackend(
                        "invalid native IVF centroid sequence".into(),
                    ));
                }
                let bytes = blob(row[3])?;
                payload.grow(bytes.len())?;
                centroids.reserve(1)?;
                let vector = blob_to_vector(bytes)?;
                read.index.validate_dimensions_sqlite(&vector)?;
                centroids.push(vector)?;
                Ok(())
            })?;
    }
    Ok(output.finish())
}

fn load_candidates(
    read: &NativeVectorRead<'_>,
    centroids: &[usize],
) -> Result<Budgeted<Candidates>> {
    let mut output = VectorBuffer::new(read)?;
    let (candidates, payload) = (&mut output.rows, &mut output.payload);
    if let Some(owner) = read.owner {
        // Release the assignment page's physical read before probing selected vector records.
        read.snapshot.visit_paged_owned_rows(
            Family::IVFAssignments,
            owner,
            &[read.field()],
            |row| {
                let centroid = usize::try_from(integer(row[4])?).map_err(|_| {
                    SQLiteError::StorageBackend("invalid native IVF assignment".into())
                })?;
                if !centroids.contains(&centroid) {
                    return Ok(true);
                }
                let doc = decode_doc_id(integer(row[2])?)?;
                read.snapshot.read_row(
                    Family::Vectors,
                    owner,
                    &[read.field(), row[2], row[3]],
                    |vector_row| {
                        let bytes = blob(vector_row[4])?;
                        payload.grow(bytes.len())?;
                        candidates.reserve(1)?;
                        let vector = blob_to_vector(bytes)?;
                        read.index.validate_dimensions_sqlite(&vector)?;
                        candidates.push((doc, vector))?;
                        Ok(())
                    },
                )?;
                Ok(true)
            },
        )?;
    }
    Ok(output.finish())
}
