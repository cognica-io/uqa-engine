//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete positional graph payloads with independent frequency and normalization length.

use uqa_core::TokenOffsets;

use super::{
    cluster_id, corrupt, decode_all_scores, encode_scores, position_entries, put_u32, put_varint,
    read_varint, validate_header, validate_scores, DocId, PostingScore, StorageBackendResult,
    TokenOccurrence, OCCURRENCE_FORMAT_VERSION, POSITIONS_MAGIC, SCORE_MAGIC,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrencePosting {
    pub doc_id: DocId,
    pub doc_length: u64,
    pub occurrences: Vec<TokenOccurrence>,
}

impl OccurrencePosting {
    pub fn score(&self) -> PostingScore {
        PostingScore {
            doc_id: self.doc_id,
            term_freq: self.occurrences.len() as u64,
            doc_length: self.doc_length,
        }
    }

    /// Compatibility projection; occurrence frequency and graph edges remain in `occurrences`.
    pub fn positions(&self) -> Vec<u32> {
        let mut positions: Vec<_> = self.occurrences.iter().map(|item| item.position).collect();
        positions.sort_unstable();
        positions.dedup();
        positions
    }
}

pub fn encode_occurrence_cluster(
    entries: &[OccurrencePosting],
) -> StorageBackendResult<(Vec<u8>, Vec<u8>)> {
    let Some(first) = entries.first() else {
        return Err(corrupt("cannot encode an empty occurrence cluster"));
    };
    let mut scores = Vec::with_capacity(entries.len());
    let mut payload = Vec::new();
    let mut offsets = vec![0_usize];
    for entry in entries {
        if cluster_id(entry.doc_id) != cluster_id(first.doc_id) {
            return Err(corrupt("one occurrence value spans multiple clusters"));
        }
        let mut previous = 0_u32;
        for occurrence in &entry.occurrences {
            occurrence
                .validate()
                .map_err(|error| corrupt(error.to_string()))?;
            let delta = occurrence
                .position
                .checked_sub(previous)
                .ok_or_else(|| corrupt("occurrence positions are not ordered"))?;
            put_varint(&mut payload, u64::from(delta));
            put_varint(&mut payload, u64::from(occurrence.position_length));
            payload.push(u8::from(occurrence.offsets.is_some()));
            if let Some(offsets) = occurrence.offsets {
                put_varint(&mut payload, offsets.start_utf8);
                put_varint(&mut payload, offsets.end_utf8 - offsets.start_utf8);
                put_varint(&mut payload, offsets.start_utf16);
                put_varint(&mut payload, offsets.end_utf16 - offsets.start_utf16);
            }
            previous = occurrence.position;
        }
        scores.push(entry.score());
        offsets.push(payload.len());
    }
    validate_scores(&scores)?;
    let score_blob = encode_scores(&scores, OCCURRENCE_FORMAT_VERSION)?;
    let mut blob = Vec::new();
    blob.extend_from_slice(POSITIONS_MAGIC);
    blob.extend_from_slice(&[OCCURRENCE_FORMAT_VERSION, 0, 0, 0]);
    put_u32(&mut blob, entries.len(), "occurrence posting count")?;
    put_u32(&mut blob, offsets.len(), "occurrence offset count")?;
    for offset in offsets {
        put_u32(&mut blob, offset, "occurrence payload offset")?;
    }
    blob.extend_from_slice(&payload);
    Ok((score_blob, blob))
}

/// Read complete occurrence payloads. Legacy positions require rebuilding from source, not inferred edges.
pub fn decode_occurrence_cluster(
    cluster_id: u64,
    score_blob: &[u8],
    positions_blob: &[u8],
) -> StorageBackendResult<Vec<OccurrencePosting>> {
    validate_header(score_blob, *SCORE_MAGIC)?;
    validate_header(positions_blob, *POSITIONS_MAGIC)?;
    if score_blob[4] != OCCURRENCE_FORMAT_VERSION || positions_blob[4] != OCCURRENCE_FORMAT_VERSION
    {
        return Err(corrupt(
            "legacy positional data requires an atomic source rebuild",
        ));
    }
    let scores = decode_all_scores(cluster_id, score_blob)?;
    let slices = position_entries(positions_blob, scores.len())?;
    scores
        .into_iter()
        .zip(slices)
        .map(|(score, bytes)| {
            let occurrences = decode_entry(bytes, score.term_freq)?;
            Ok(OccurrencePosting {
                doc_id: score.doc_id,
                doc_length: score.doc_length,
                occurrences,
            })
        })
        .collect()
}

fn decode_entry(mut bytes: &[u8], frequency: u64) -> StorageBackendResult<Vec<TokenOccurrence>> {
    let count =
        usize::try_from(frequency).map_err(|_| corrupt("occurrence count exceeds memory"))?;
    if count > bytes.len() / 3 {
        return Err(corrupt("occurrence count exceeds payload length"));
    }
    let mut occurrences = Vec::with_capacity(count);
    let mut position = 0_u32;
    for _ in 0..count {
        let delta = u32::try_from(read_varint(&mut bytes)?)
            .map_err(|_| corrupt("occurrence position delta exceeds u32"))?;
        position = position
            .checked_add(delta)
            .ok_or_else(|| corrupt("occurrence position overflow"))?;
        let position_length = u32::try_from(read_varint(&mut bytes)?)
            .map_err(|_| corrupt("occurrence position length exceeds u32"))?;
        let Some((&flags, rest)) = bytes.split_first() else {
            return Err(corrupt("missing occurrence flags"));
        };
        bytes = rest;
        let offsets = match flags {
            0 => None,
            1 => Some(read_offsets(&mut bytes)?),
            _ => return Err(corrupt("unknown occurrence flags")),
        };
        let occurrence = TokenOccurrence {
            position,
            position_length,
            offsets,
        };
        occurrence
            .validate()
            .map_err(|error| corrupt(error.to_string()))?;
        occurrences.push(occurrence);
    }
    if !bytes.is_empty() {
        return Err(corrupt("occurrence entry contains trailing bytes"));
    }
    Ok(occurrences)
}

fn read_offsets(bytes: &mut &[u8]) -> StorageBackendResult<TokenOffsets> {
    fn range(bytes: &mut &[u8]) -> StorageBackendResult<(u64, u64)> {
        let start = read_varint(bytes)?;
        let end = start
            .checked_add(read_varint(bytes)?)
            .ok_or_else(|| corrupt("occurrence source offset overflow"))?;
        Ok((start, end))
    }
    let (start_utf8, end_utf8) = range(bytes)?;
    let (start_utf16, end_utf16) = range(bytes)?;
    Ok(TokenOffsets {
        start_utf8,
        end_utf8,
        start_utf16,
        end_utf16,
    })
}
