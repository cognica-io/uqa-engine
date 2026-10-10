//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A cluster encoded from its postings one at a time in document order, so that the postings need not be collected first.

use uqa_core::memory::BudgetedVec;

use super::super::encoding::{put_u32, put_varint};
use super::super::scores::ScoreEncoder;
use super::super::{
    cluster_id, corrupt, read_varint, DocId, PostingScore, StorageBackendResult, TokenOccurrence,
    HEADER_LEN, OCCURRENCE_FORMAT_VERSION, POSITIONS_MAGIC,
};
use crate::read_control::StorageReadControl;

/// The score and positions values of one occurrence cluster, built from its postings in ascending document order.
pub struct OccurrenceClusterBuilder {
    scores: ScoreEncoder,
    payload: BudgetedVec<u8>,
    /// The end of each posting's occurrences in `payload`, after a leading zero.
    offsets: BudgetedVec<usize>,
    cluster: Option<u64>,
}

impl OccurrenceClusterBuilder {
    pub fn new(control: &StorageReadControl) -> StorageBackendResult<Self> {
        let mut offsets = BudgetedVec::new(control.memory());
        offsets.push(0_usize)?;
        Ok(Self {
            scores: ScoreEncoder::new(control),
            payload: BudgetedVec::new(control.memory()),
            offsets,
            cluster: None,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// Reserve room for `count` more postings.
    pub fn reserve(&mut self, count: usize) -> StorageBackendResult<()> {
        self.scores.reserve(count)?;
        self.offsets.reserve(count)?;
        Ok(())
    }

    /// Add the posting of document `doc_id`, `doc_length` tokens long, holding `occurrences`.
    pub fn push(
        &mut self,
        doc_id: DocId,
        doc_length: u64,
        occurrences: &[TokenOccurrence],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.enter(doc_id)?;
        encode_occurrences(occurrences, &mut self.payload, control)?;
        self.close(doc_id, occurrences.len() as u64, doc_length)
    }

    /// Add the posting of document `doc_id` that [`encode_posting`] encoded.
    pub(crate) fn push_encoded(
        &mut self,
        doc_id: DocId,
        mut posting: &[u8],
    ) -> StorageBackendResult<()> {
        let doc_length = read_varint(&mut posting)?;
        let term_freq = read_varint(&mut posting)?;
        self.enter(doc_id)?;
        self.payload.extend_from_slice(posting)?;
        self.close(doc_id, term_freq, doc_length)
    }

    fn enter(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let cluster = cluster_id(doc_id);
        if self.cluster.is_some_and(|current| current != cluster) {
            return Err(corrupt("one occurrence value spans multiple clusters"));
        }
        self.cluster = Some(cluster);
        Ok(())
    }

    fn close(
        &mut self,
        doc_id: DocId,
        term_freq: u64,
        doc_length: u64,
    ) -> StorageBackendResult<()> {
        self.scores.push(PostingScore {
            doc_id,
            term_freq,
            doc_length,
        })?;
        self.offsets.push(self.payload.len())?;
        Ok(())
    }

    /// The cluster's score and positions values, each retaining its reservation until dropped.
    pub fn finish(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(BudgetedVec<u8>, BudgetedVec<u8>)> {
        control.cancellation().check()?;
        if self.scores.is_empty() {
            return Err(corrupt("cannot encode an empty occurrence cluster"));
        }
        let count = self.scores.len();
        let score_blob = self.scores.finish(OCCURRENCE_FORMAT_VERSION, control)?;
        let mut blob = BudgetedVec::new(control.memory());
        let blob_len = self
            .offsets
            .len()
            .checked_mul(size_of::<u32>())
            .and_then(|directory| HEADER_LEN.checked_add(directory))
            .and_then(|header| header.checked_add(self.payload.len()))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        blob.reserve(blob_len)?;
        blob.extend_from_slice(POSITIONS_MAGIC)?;
        blob.extend_from_slice(&[OCCURRENCE_FORMAT_VERSION, 0, 0, 0])?;
        put_u32(&mut blob, count, "occurrence posting count")?;
        put_u32(&mut blob, self.offsets.len(), "occurrence offset count")?;
        for &offset in self.offsets.iter() {
            put_u32(&mut blob, offset, "occurrence payload offset")?;
        }
        blob.extend_from_slice(&self.payload)?;
        Ok((score_blob, blob))
    }
}

/// Encode a posting of a document `doc_length` tokens long holding `occurrences`, as [`OccurrenceClusterBuilder::push_encoded`] reads it.
pub(crate) fn encode_posting(
    doc_length: u64,
    occurrences: &[TokenOccurrence],
    output: &mut BudgetedVec<u8>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    put_varint(output, doc_length)?;
    put_varint(output, occurrences.len() as u64)?;
    encode_occurrences(occurrences, output, control)
}

/// Encode one posting's occurrences as the positions value holds them: each position as the distance from the previous one, its length and its offsets.
fn encode_occurrences(
    occurrences: &[TokenOccurrence],
    payload: &mut BudgetedVec<u8>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut previous = 0_u32;
    for occurrence in occurrences {
        control.cancellation().check()?;
        occurrence
            .validate()
            .map_err(|error| corrupt(error.to_string()))?;
        let delta = occurrence
            .position
            .checked_sub(previous)
            .ok_or_else(|| corrupt("occurrence positions are not ordered"))?;
        put_varint(payload, u64::from(delta))?;
        put_varint(payload, u64::from(occurrence.position_length))?;
        payload.push(u8::from(occurrence.offsets.is_some()))?;
        if let Some(offsets) = occurrence.offsets {
            put_varint(payload, offsets.start_utf8)?;
            put_varint(payload, offsets.end_utf8 - offsets.start_utf8)?;
            put_varint(payload, offsets.start_utf16)?;
            put_varint(payload, offsets.end_utf16 - offsets.start_utf16)?;
        }
        previous = occurrence.position;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
