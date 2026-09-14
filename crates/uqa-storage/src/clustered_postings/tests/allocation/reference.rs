//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocating decoder pinned to commit 5c021219 for independent format and corruption differentials.

use super::*;

pub(super) fn parse_score_blob(blob: &[u8]) -> StorageBackendResult<(usize, Vec<ScoreBlock>)> {
    validate_header(blob, *SCORE_MAGIC)?;
    let count = read_u32(blob, 8)? as usize;
    if count == 0 {
        return Err(corrupt("persisted posting cluster is empty"));
    }
    let block_count = read_u32(blob, 12)? as usize;
    if block_count != count.div_ceil(DEFAULT_BLOCK_SIZE) {
        return Err(corrupt("score block count does not match posting count"));
    }
    let directory_end = HEADER_LEN
        .checked_add(
            block_count
                .checked_mul(SCORE_DIRECTORY_ENTRY_LEN)
                .ok_or_else(|| corrupt("score directory size overflow"))?,
        )
        .ok_or_else(|| corrupt("score directory end overflow"))?;
    if directory_end > blob.len() {
        return Err(corrupt("truncated score block directory"));
    }
    let minimum_stream_bytes = count
        .checked_mul(3)
        .ok_or_else(|| corrupt("minimum score stream size overflow"))?;
    if directory_end
        .checked_add(minimum_stream_bytes)
        .is_none_or(|minimum_len| minimum_len > blob.len())
    {
        return Err(corrupt("posting count exceeds score stream length"));
    }

    let mut blocks = Vec::with_capacity(block_count);
    let mut total = 0_usize;
    let mut previous_last = None;
    let mut previous_end = directory_end;
    for index in 0..block_count {
        let start = HEADER_LEN + index * SCORE_DIRECTORY_ENTRY_LEN;
        let block = ScoreBlock {
            version: blob[4],
            count: usize::from(read_u16(blob, start)?),
            last_offset: read_u16(blob, start + 2)?,
            docs_start: read_u32(blob, start + 4)? as usize,
            docs_end: read_u32(blob, start + 8)? as usize,
            term_freqs_start: read_u32(blob, start + 12)? as usize,
            term_freqs_end: read_u32(blob, start + 16)? as usize,
            doc_lengths_start: read_u32(blob, start + 20)? as usize,
            doc_lengths_end: read_u32(blob, start + 24)? as usize,
        };
        let expected_count = (count - total).min(DEFAULT_BLOCK_SIZE);
        if block.count != expected_count || block.count == 0 {
            return Err(corrupt("invalid score block posting count"));
        }
        if previous_last.is_some_and(|last| last >= block.last_offset) {
            return Err(corrupt("score block document ranges overlap"));
        }
        if block.docs_start != previous_end
            || block.docs_start > block.docs_end
            || block.docs_end != block.term_freqs_start
            || block.term_freqs_start > block.term_freqs_end
            || block.term_freqs_end != block.doc_lengths_start
            || block.doc_lengths_start > block.doc_lengths_end
            || block.doc_lengths_end > blob.len()
        {
            return Err(corrupt("invalid score stream boundaries"));
        }
        let mut encoded_docs = &blob[block.docs_start..block.docs_end];
        let first_offset = u16::try_from(read_varint(&mut encoded_docs)?)
            .map_err(|_| corrupt("first document offset exceeds clustered format"))?;
        if first_offset > block.last_offset
            || previous_last.is_some_and(|last| last >= first_offset)
        {
            return Err(corrupt("score block document ranges overlap"));
        }
        total = total
            .checked_add(block.count)
            .ok_or_else(|| corrupt("score posting count overflow"))?;
        previous_last = Some(block.last_offset);
        previous_end = block.doc_lengths_end;
        blocks.push(block);
    }
    if total != count || previous_end != blob.len() {
        return Err(corrupt("score blob length or posting count mismatch"));
    }
    Ok((count, blocks))
}

pub(super) fn decode_score_block_into(
    blob: &[u8],
    cluster_id: u64,
    block: ScoreBlock,
    output: &mut Vec<PostingScore>,
) -> StorageBackendResult<()> {
    let base = cluster_base(cluster_id)?;
    let mut docs = &blob[block.docs_start..block.docs_end];
    let mut term_freqs = &blob[block.term_freqs_start..block.term_freqs_end];
    let mut doc_lengths = &blob[block.doc_lengths_start..block.doc_lengths_end];
    let mut previous = 0_u16;
    for index in 0..block.count {
        let encoded = read_varint(&mut docs)?;
        let delta = u16::try_from(encoded)
            .map_err(|_| corrupt("document delta exceeds clustered format"))?;
        let offset = if index == 0 {
            delta
        } else {
            previous
                .checked_add(delta)
                .ok_or_else(|| corrupt("document offset overflow"))?
        };
        if index > 0 && offset <= previous {
            return Err(corrupt(
                "posting document offsets are not strictly increasing",
            ));
        }
        let term_freq = read_varint(&mut term_freqs)?;
        let doc_length = read_varint(&mut doc_lengths)?;
        if term_freq == 0
            || doc_length == 0
            || (block.version == FORMAT_VERSION && doc_length < term_freq)
        {
            return Err(corrupt("invalid term frequency or document length"));
        }
        output.push(PostingScore {
            doc_id: base
                .checked_add(u64::from(offset))
                .ok_or_else(|| corrupt("posting document id overflow"))?,
            term_freq,
            doc_length,
        });
        previous = offset;
    }
    if !docs.is_empty() || !term_freqs.is_empty() || !doc_lengths.is_empty() {
        return Err(corrupt("score stream contains trailing bytes"));
    }
    if previous != block.last_offset {
        return Err(corrupt(
            "score block last document does not match directory",
        ));
    }
    Ok(())
}

pub fn decode_all_scores(
    cluster_id: u64,
    score_blob: &[u8],
) -> StorageBackendResult<Vec<PostingScore>> {
    let (count, blocks) = parse_score_blob(score_blob)?;
    let mut entries = Vec::with_capacity(count);
    for block in blocks {
        decode_score_block_into(score_blob, cluster_id, block, &mut entries)?;
    }
    validate_scores(&entries)?;
    Ok(entries)
}

fn position_entries(blob: &[u8], expected_count: usize) -> StorageBackendResult<Vec<&[u8]>> {
    let count = read_u32(blob, 8)? as usize;
    let offset_count = read_u32(blob, 12)? as usize;
    if count != expected_count || offset_count != count.saturating_add(1) {
        return Err(corrupt("positions posting count mismatch"));
    }
    let data_start = HEADER_LEN
        .checked_add(
            offset_count
                .checked_mul(std::mem::size_of::<u32>())
                .ok_or_else(|| corrupt("positions offset table size overflow"))?,
        )
        .ok_or_else(|| corrupt("positions data offset overflow"))?;
    if data_start > blob.len() {
        return Err(corrupt("truncated positions offset table"));
    }
    let data = &blob[data_start..];
    let mut offsets = Vec::with_capacity(offset_count);
    for index in 0..offset_count {
        offsets.push(read_u32(blob, HEADER_LEN + index * 4)? as usize);
    }
    if offsets.first().copied() != Some(0)
        || offsets.last().copied() != Some(data.len())
        || offsets.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Err(corrupt("invalid positions payload offsets"));
    }

    Ok(offsets
        .windows(2)
        .map(|pair| &data[pair[0]..pair[1]])
        .collect())
}

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
