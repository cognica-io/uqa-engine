//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF canonical values and persisted metadata share one native read and mutation boundary.

use uqa_core::{memory::Budgeted, PostingList};
use uqa_storage::{
    ivf_index::{IVFMutation, IVFState},
    vector_index::{
        cosine_similarity,
        query::{nearest_centroids, SpillingVectorScores},
    },
    KeyValueBatch,
};

mod publication;
mod records;
mod state;
#[cfg(test)]
mod tests;
pub(super) use publication::{drop_metadata, write_prepared};
pub(crate) use records::NativeIVFRecords;
pub(super) use state::load_state;

use super::{
    metadata::{decode_metadata, SQLiteIVFMeta},
    SQLiteIVFIndex,
};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::{
    blob_to_vector, decode_doc_id,
    native::{blob, integer, text, NativeVectorRead, VectorBuffer},
};
use crate::{Result, SQLiteError};

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
        let doc_id = decode_doc_id(doc)?;
        let metadata = load_state(&read, self.params, false)?.prepare(&[IVFMutation::Replace {
            document: doc_id,
            vectors,
        }])?;
        read.replace(batch, doc, encoded)?;
        publication::write_input(
            &read,
            batch,
            &metadata,
            IVFMutation::Replace {
                document: doc_id,
                vectors,
            },
        )
    }

    pub(super) fn delete_native(
        &self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        doc: i64,
    ) -> Result<()> {
        let doc_id = decode_doc_id(doc)?;
        let metadata =
            load_state(read, self.params, false)?.prepare(&[IVFMutation::Delete(doc_id)])?;
        if read.owner.is_none() {
            return Ok(());
        }
        read.delete(batch, doc)?;
        publication::write_input(read, batch, &metadata, IVFMutation::Delete(doc_id))
    }

    pub(super) fn search_native(
        &self,
        read: &NativeVectorRead<'_>,
        query: &[f32],
        k: usize,
    ) -> Result<PostingList> {
        // The metadata and canonical count must be observed at this same retained boundary.
        let count = read.count()?;
        let Some(meta) = load_metadata(read)? else {
            if count == 0 {
                return Ok(PostingList::new());
            }
            return Err(SQLiteError::StorageBackend(
                "missing native IVF metadata".into(),
            ));
        };
        if meta.dimensions != self.persistent.dimensions
            || meta.params != self.params
            || meta.vector_count != count
        {
            return Err(SQLiteError::StorageBackend(
                "native IVF metadata does not match its canonical generation".into(),
            ));
        }
        if meta.state != IVFState::Trained {
            return exact(read, query, k);
        }
        let centroids = load_centroids(read)?;
        if centroids.is_empty() {
            return Err(SQLiteError::StorageBackend(
                "trained native IVF index has no centroids".into(),
            ));
        }
        let probes = nearest_centroids(
            query,
            &centroids,
            self.params.nprobe,
            Some(&read.snapshot.control),
        )?;
        let mut scores = SpillingVectorScores::new(&read.snapshot.control);
        load_candidates(
            read,
            &probes,
            centroids.len(),
            meta.vector_count,
            query,
            &mut scores,
        )?;
        Ok(scores.finish(k)?)
    }
}

fn exact(read: &NativeVectorRead<'_>, query: &[f32], k: usize) -> Result<PostingList> {
    let mut scores = SpillingVectorScores::new(&read.snapshot.control);
    read.visit_ordered_vectors(|document, _, vector| {
        scores.add(document, cosine_similarity(query, vector))?;
        Ok(())
    })?;
    Ok(scores.finish(k)?)
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
    centroid_count: usize,
    expected_count: usize,
    query: &[f32],
    scores: &mut SpillingVectorScores,
) -> Result<()> {
    let mut assignment_count = 0_usize;
    if let Some(owner) = read.owner {
        // Release the assignment page's physical read before probing selected vector records.
        read.snapshot.visit_paged_owned_rows(
            Family::IVFAssignments,
            owner,
            &[read.field()],
            |row| {
                assignment_count = assignment_count
                    .checked_add(1)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
                let centroid = usize::try_from(integer(row[4])?).map_err(|_| {
                    SQLiteError::StorageBackend("invalid native IVF assignment".into())
                })?;
                if centroid >= centroid_count {
                    return Err(SQLiteError::StorageBackend(
                        "native IVF assignment references a missing centroid".into(),
                    ));
                }
                if !centroids.contains(&centroid) {
                    return Ok(true);
                }
                let doc = decode_doc_id(integer(row[2])?)?;
                read.snapshot
                    .read_row(
                        Family::Vectors,
                        owner,
                        &[read.field(), row[2], row[3]],
                        |vector_row| {
                            let bytes = blob(vector_row[4])?;
                            let _memory = read.snapshot.control.memory().reserve(bytes.len())?;
                            let vector = blob_to_vector(bytes)?;
                            read.index.validate_dimensions_sqlite(&vector)?;
                            scores.add(doc, cosine_similarity(query, &vector))?;
                            Ok(())
                        },
                    )?
                    .ok_or_else(|| {
                        SQLiteError::StorageBackend(
                            "native IVF assignment references a missing vector".into(),
                        )
                    })?;
                Ok(true)
            },
        )?;
    }
    if assignment_count != expected_count {
        return Err(SQLiteError::StorageBackend(
            "native IVF assignments do not cover the canonical generation".into(),
        ));
    }
    Ok(())
}
