//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing linear position and scalar reverse-term formats, retained for validated migration reads.

use super::{
    corrupt, position_entries, put_u32, put_varint, read_u32, read_varint, validate_header,
    ClusterPosting, PostingScore, StorageBackendResult, FORMAT_VERSION, HEADER_LEN,
    POSITIONS_MAGIC, TERMS_MAGIC,
};

pub fn encode_terms(terms: &[String]) -> StorageBackendResult<Vec<u8>> {
    if terms.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(corrupt("document terms are not strictly ordered"));
    }
    let payload_len = terms.iter().try_fold(0_usize, |total, term| {
        total
            .checked_add(4)
            .and_then(|value| value.checked_add(term.len()))
            .ok_or_else(|| corrupt("document term payload size overflow"))
    })?;
    let mut output = Vec::with_capacity(
        12_usize
            .checked_add(payload_len)
            .ok_or_else(|| corrupt("document term payload size overflow"))?,
    );
    output.extend_from_slice(TERMS_MAGIC);
    output.push(FORMAT_VERSION);
    output.extend_from_slice(&[0; 3]);
    put_u32(&mut output, terms.len(), "document term count")?;
    for term in terms {
        put_u32(&mut output, term.len(), "document term length")?;
        output.extend_from_slice(term.as_bytes());
    }
    Ok(output)
}

pub fn decode_terms(blob: &[u8]) -> StorageBackendResult<Vec<String>> {
    if blob.len() < 12 || blob.get(..4) != Some(TERMS_MAGIC.as_slice()) {
        return Err(corrupt("missing document term header"));
    }
    if blob[4] != FORMAT_VERSION || blob[5..8] != [0; 3] {
        return Err(corrupt("unsupported document term format"));
    }
    let count = read_u32(blob, 8)? as usize;
    let minimum_len = 12_usize
        .checked_add(
            count
                .checked_mul(std::mem::size_of::<u32>())
                .ok_or_else(|| corrupt("document term length table size overflow"))?,
        )
        .ok_or_else(|| corrupt("document term minimum size overflow"))?;
    if minimum_len > blob.len() {
        return Err(corrupt("document term count exceeds payload length"));
    }
    let mut offset = 12_usize;
    let mut terms = Vec::with_capacity(count);
    for _ in 0..count {
        let length = read_u32(blob, offset)? as usize;
        offset = offset
            .checked_add(4)
            .ok_or_else(|| corrupt("document term offset overflow"))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| corrupt("document term end overflow"))?;
        let bytes = blob
            .get(offset..end)
            .ok_or_else(|| corrupt("truncated document term"))?;
        let term = std::str::from_utf8(bytes)
            .map_err(|error| corrupt(format!("document term is not UTF-8: {error}")))?;
        terms.push(term.to_string());
        offset = end;
    }
    if offset != blob.len() || terms.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(corrupt("invalid document term ordering or trailing bytes"));
    }
    Ok(terms)
}

pub(super) fn encode_positions(entries: &[ClusterPosting]) -> StorageBackendResult<Vec<u8>> {
    let offsets_bytes = entries
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(std::mem::size_of::<u32>()))
        .ok_or_else(|| corrupt("positions offset table size overflow"))?;
    let data_start = HEADER_LEN
        .checked_add(offsets_bytes)
        .ok_or_else(|| corrupt("positions blob size overflow"))?;
    let mut data = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len() + 1);
    offsets.push(0_u32);
    for entry in entries {
        let mut previous = 0_u32;
        for (index, position) in entry.positions.iter().copied().enumerate() {
            let delta = if index == 0 {
                position
            } else {
                position
                    .checked_sub(previous)
                    .ok_or_else(|| corrupt("term positions are not sorted"))?
            };
            put_varint(&mut data, u64::from(delta));
            previous = position;
        }
        offsets.push(
            u32::try_from(data.len())
                .map_err(|_| corrupt("positions payload exceeds the u32 format"))?,
        );
    }

    let mut output = Vec::with_capacity(
        data_start
            .checked_add(data.len())
            .ok_or_else(|| corrupt("positions blob size overflow"))?,
    );
    output.extend_from_slice(POSITIONS_MAGIC);
    output.push(FORMAT_VERSION);
    output.extend_from_slice(&[0; 3]);
    put_u32(&mut output, entries.len(), "positions posting count")?;
    put_u32(&mut output, entries.len() + 1, "positions offset count")?;
    for offset in offsets {
        output.extend_from_slice(&offset.to_le_bytes());
    }
    output.extend_from_slice(&data);
    Ok(output)
}

pub(super) fn decode_positions(
    blob: &[u8],
    scores: &[PostingScore],
) -> StorageBackendResult<Vec<Vec<u32>>> {
    validate_header(blob, *POSITIONS_MAGIC)?;
    if blob[4] != FORMAT_VERSION {
        return Err(corrupt("legacy positions require format version 1"));
    }
    let slices = position_entries(blob, scores.len())?;
    let mut all = Vec::with_capacity(scores.len());
    for (score, mut encoded) in scores.iter().zip(slices) {
        let position_count = usize::try_from(score.term_freq)
            .map_err(|_| corrupt("term frequency exceeds addressable memory"))?;
        if position_count > encoded.len() {
            return Err(corrupt("term frequency exceeds positions payload length"));
        }
        let mut positions = Vec::with_capacity(position_count);
        let mut previous = 0_u32;
        for ordinal in 0..position_count {
            let delta = u32::try_from(read_varint(&mut encoded)?)
                .map_err(|_| corrupt("term-position delta exceeds u32"))?;
            let position = if ordinal == 0 {
                delta
            } else {
                previous
                    .checked_add(delta)
                    .ok_or_else(|| corrupt("term position overflow"))?
            };
            if ordinal > 0 && position <= previous {
                return Err(corrupt("term positions are not strictly increasing"));
            }
            positions.push(position);
            previous = position;
        }
        if !encoded.is_empty() {
            return Err(corrupt("positions entry contains trailing bytes"));
        }
        all.push(positions);
    }
    Ok(all)
}
