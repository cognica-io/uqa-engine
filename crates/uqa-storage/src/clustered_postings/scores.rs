//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Columnar score streams; graph frequencies and overlap-discounted lengths remain independent.

use super::{
    cluster_base, cluster_offset, corrupt, put_u32, put_varint, read_u16, read_u32, read_varint,
    validate_header, PostingScore, ScoreBlock, StorageBackendResult, DEFAULT_BLOCK_SIZE,
    FORMAT_VERSION, HEADER_LEN, SCORE_DIRECTORY_ENTRY_LEN, SCORE_MAGIC,
};

pub(super) fn encode_scores(
    entries: &[PostingScore],
    version: u8,
) -> StorageBackendResult<Vec<u8>> {
    struct EncodedBlock {
        count: u16,
        last_offset: u16,
        docs: Vec<u8>,
        term_freqs: Vec<u8>,
        doc_lengths: Vec<u8>,
    }

    let mut blocks = Vec::with_capacity(entries.len().div_ceil(DEFAULT_BLOCK_SIZE));
    for chunk in entries.chunks(DEFAULT_BLOCK_SIZE) {
        let mut docs = Vec::new();
        let mut term_freqs = Vec::new();
        let mut doc_lengths = Vec::new();
        let mut previous = 0_u16;
        for (index, entry) in chunk.iter().enumerate() {
            let offset = cluster_offset(entry.doc_id)?;
            let delta = if index == 0 {
                u64::from(offset)
            } else {
                u64::from(offset.checked_sub(previous).ok_or_else(|| {
                    corrupt("posting document offsets are not strictly increasing")
                })?)
            };
            put_varint(&mut docs, delta);
            put_varint(&mut term_freqs, entry.term_freq);
            put_varint(&mut doc_lengths, entry.doc_length);
            previous = offset;
        }
        blocks.push(EncodedBlock {
            count: u16::try_from(chunk.len())
                .map_err(|_| corrupt("score block contains too many postings"))?,
            last_offset: cluster_offset(chunk.last().expect("non-empty score block").doc_id)?,
            docs,
            term_freqs,
            doc_lengths,
        });
    }

    let directory_bytes = blocks
        .len()
        .checked_mul(SCORE_DIRECTORY_ENTRY_LEN)
        .ok_or_else(|| corrupt("score directory size overflow"))?;
    let data_start = HEADER_LEN
        .checked_add(directory_bytes)
        .ok_or_else(|| corrupt("score blob size overflow"))?;
    let data_bytes = blocks.iter().try_fold(0_usize, |total, block| {
        total
            .checked_add(block.docs.len())
            .and_then(|value| value.checked_add(block.term_freqs.len()))
            .and_then(|value| value.checked_add(block.doc_lengths.len()))
            .ok_or_else(|| corrupt("score blob size overflow"))
    })?;
    let mut output = Vec::with_capacity(
        data_start
            .checked_add(data_bytes)
            .ok_or_else(|| corrupt("score blob size overflow"))?,
    );
    output.extend_from_slice(SCORE_MAGIC);
    output.push(version);
    output.extend_from_slice(&[0; 3]);
    put_u32(&mut output, entries.len(), "posting count")?;
    put_u32(&mut output, blocks.len(), "score block count")?;

    let mut offset = data_start;
    for block in &blocks {
        output.extend_from_slice(&block.count.to_le_bytes());
        output.extend_from_slice(&block.last_offset.to_le_bytes());
        put_u32(&mut output, offset, "document stream offset")?;
        offset = offset
            .checked_add(block.docs.len())
            .ok_or_else(|| corrupt("score stream offset overflow"))?;
        put_u32(&mut output, offset, "document stream end")?;
        put_u32(&mut output, offset, "term-frequency stream offset")?;
        offset = offset
            .checked_add(block.term_freqs.len())
            .ok_or_else(|| corrupt("score stream offset overflow"))?;
        put_u32(&mut output, offset, "term-frequency stream end")?;
        put_u32(&mut output, offset, "document-length stream offset")?;
        offset = offset
            .checked_add(block.doc_lengths.len())
            .ok_or_else(|| corrupt("score stream offset overflow"))?;
        put_u32(&mut output, offset, "document-length stream end")?;
    }
    for block in blocks {
        output.extend_from_slice(&block.docs);
        output.extend_from_slice(&block.term_freqs);
        output.extend_from_slice(&block.doc_lengths);
    }
    Ok(output)
}

/// Validated directory borrowed from its encoded owner; no block table is allocated.
pub(super) struct ScoreDirectory<'a> {
    blob: &'a [u8],
    pub count: usize,
    pub blocks: usize,
}
impl<'a> ScoreDirectory<'a> {
    pub fn new(
        blob: &'a [u8],
        poll: &mut dyn FnMut() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<Self> {
        poll()?;
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

        let mut total = 0_usize;
        let mut previous_last = None;
        let mut previous_end = directory_end;
        for index in 0..block_count {
            poll()?;
            let block = read_score_block(blob, index)?;
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
        }
        if total != count || previous_end != blob.len() {
            return Err(corrupt("score blob length or posting count mismatch"));
        }
        poll()?;
        Ok(Self {
            blob,
            count,
            blocks: block_count,
        })
    }
    pub fn block(&self, index: usize) -> StorageBackendResult<ScoreBlock> {
        if index >= self.blocks {
            return Err(corrupt("score block index is out of bounds"));
        }
        read_score_block(self.blob, index)
    }
}

fn read_score_block(blob: &[u8], index: usize) -> StorageBackendResult<ScoreBlock> {
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
    Ok(block)
}

pub(super) fn parse_score_blob(blob: &[u8]) -> StorageBackendResult<(usize, Vec<ScoreBlock>)> {
    let directory = ScoreDirectory::new(blob, &mut || Ok(()))?;
    let mut blocks = Vec::with_capacity(directory.blocks);
    for index in 0..directory.blocks {
        blocks.push(directory.block(index)?);
    }
    Ok((directory.count, blocks))
}

pub(super) fn decode_score_block_into(
    blob: &[u8],
    cluster_id: u64,
    block: ScoreBlock,
    output: &mut Vec<PostingScore>,
) -> StorageBackendResult<()> {
    visit_score_block(blob, cluster_id, block, &mut || Ok(()), |entry, _| {
        output.push(entry);
        Ok(())
    })
}

pub(super) fn visit_score_block(
    blob: &[u8],
    cluster_id: u64,
    block: ScoreBlock,
    poll: &mut dyn FnMut() -> StorageBackendResult<()>,
    mut visit: impl FnMut(
        PostingScore,
        &mut dyn FnMut() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    poll()?;
    let base = cluster_base(cluster_id)?;
    let mut docs = &blob[block.docs_start..block.docs_end];
    let mut term_freqs = &blob[block.term_freqs_start..block.term_freqs_end];
    let mut doc_lengths = &blob[block.doc_lengths_start..block.doc_lengths_end];
    let mut previous = 0_u16;
    for index in 0..block.count {
        poll()?;
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
        visit(
            PostingScore {
                doc_id: base
                    .checked_add(u64::from(offset))
                    .ok_or_else(|| corrupt("posting document id overflow"))?,
                term_freq,
                doc_length,
            },
            poll,
        )?;
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
    poll()?;
    Ok(())
}
