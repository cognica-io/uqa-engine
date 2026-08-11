//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backend-neutral clustered posting codec and lazy score cursor.
//!
//! A posting key identifies `(table, field, term, cluster_id)`, where one
//! cluster covers 2^16 consecutive document identifiers. Score data and
//! positional payloads are encoded separately: ranking reads only the
//! columnar document-offset, term-frequency, and document-length streams,
//! while phrase/highlight consumers opt into the positions blob.

use std::sync::Arc;

use uqa_core::DocId;

use crate::{StorageBackendError, StorageBackendResult, DEFAULT_BLOCK_SIZE};

pub const POSTING_CLUSTER_DOCS: u64 = 1 << 16;

const SCORE_MAGIC: &[u8; 4] = b"UQCS";
const POSITIONS_MAGIC: &[u8; 4] = b"UQCP";
const TERMS_MAGIC: &[u8; 4] = b"UQCT";
const FORMAT_VERSION: u8 = 1;
const HEADER_LEN: usize = 16;
const SCORE_DIRECTORY_ENTRY_LEN: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingScore {
    pub doc_id: DocId,
    pub term_freq: u64,
    pub doc_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClusterPosting {
    pub doc_id: DocId,
    pub term_freq: u64,
    pub doc_length: u64,
    pub positions: Vec<u32>,
}

pub trait PostingCursor: Send {
    fn doc_freq(&self) -> u64;
    fn ordinal(&self) -> u64;
    fn current(&self) -> Option<PostingScore>;
    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>>;
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>>;
    fn boxed_clone(&self) -> Box<dyn PostingCursor>;
}

impl Clone for Box<dyn PostingCursor> {
    fn clone(&self) -> Self {
        self.boxed_clone()
    }
}

#[derive(Clone)]
pub struct MaterializedPostingCursor {
    entries: Arc<[PostingScore]>,
    position: usize,
}

impl MaterializedPostingCursor {
    pub fn new(entries: Vec<PostingScore>) -> StorageBackendResult<Self> {
        validate_scores(&entries)?;
        Ok(Self {
            entries: entries.into(),
            position: 0,
        })
    }
}

impl PostingCursor for MaterializedPostingCursor {
    fn doc_freq(&self) -> u64 {
        self.entries.len() as u64
    }

    fn ordinal(&self) -> u64 {
        self.position as u64
    }

    fn current(&self) -> Option<PostingScore> {
        self.entries.get(self.position).copied()
    }

    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        self.position = self.position.saturating_add(1).min(self.entries.len());
        Ok(self.current())
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        if self.current().is_some_and(|entry| entry.doc_id >= target) {
            return Ok(self.current());
        }
        let relative = self.entries[self.position..].partition_point(|entry| entry.doc_id < target);
        self.position = self
            .position
            .saturating_add(relative)
            .min(self.entries.len());
        Ok(self.current())
    }

    fn boxed_clone(&self) -> Box<dyn PostingCursor> {
        Box::new(self.clone())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedScoreCluster {
    pub cluster_id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct ScoreBlock {
    count: usize,
    last_offset: u16,
    docs_start: usize,
    docs_end: usize,
    term_freqs_start: usize,
    term_freqs_end: usize,
    doc_lengths_start: usize,
    doc_lengths_end: usize,
}

#[derive(Debug, Clone, Copy)]
struct CursorBlock {
    cluster_index: usize,
    score_block: ScoreBlock,
    first_ordinal: u64,
    last_doc_id: DocId,
}

#[derive(Clone)]
pub(crate) struct ClusteredPostingCursor {
    clusters: Arc<[EncodedScoreCluster]>,
    blocks: Arc<[CursorBlock]>,
    doc_freq: u64,
    block_index: usize,
    entries: Vec<PostingScore>,
    position_in_block: usize,
}

impl ClusteredPostingCursor {
    pub(crate) fn new(clusters: Vec<EncodedScoreCluster>) -> StorageBackendResult<Self> {
        let mut previous_cluster = None;
        let mut blocks = Vec::new();
        let mut doc_freq = 0_u64;
        for (cluster_index, cluster) in clusters.iter().enumerate() {
            if previous_cluster.is_some_and(|previous| previous >= cluster.cluster_id) {
                return Err(corrupt("cluster identifiers are not strictly increasing"));
            }
            previous_cluster = Some(cluster.cluster_id);
            let (_, score_blocks) = parse_score_blob(&cluster.bytes)?;
            for score_block in score_blocks {
                let first_ordinal = doc_freq;
                doc_freq = doc_freq
                    .checked_add(score_block.count as u64)
                    .ok_or_else(|| corrupt("posting count overflow"))?;
                blocks.push(CursorBlock {
                    cluster_index,
                    score_block,
                    first_ordinal,
                    last_doc_id: cluster_base(cluster.cluster_id)?
                        .checked_add(u64::from(score_block.last_offset))
                        .ok_or_else(|| corrupt("block document id overflow"))?,
                });
            }
        }

        let mut cursor = Self {
            clusters: clusters.into(),
            blocks: blocks.into(),
            doc_freq,
            block_index: 0,
            entries: Vec::new(),
            position_in_block: 0,
        };
        if !cursor.blocks.is_empty() {
            cursor.load_block(0)?;
        }
        Ok(cursor)
    }

    fn load_block(&mut self, block_index: usize) -> StorageBackendResult<()> {
        if block_index >= self.blocks.len() {
            self.block_index = self.blocks.len();
            self.entries.clear();
            self.position_in_block = 0;
            return Ok(());
        }
        let block = self.blocks[block_index];
        let cluster = &self.clusters[block.cluster_index];
        self.entries.clear();
        self.entries.reserve(block.score_block.count);
        decode_score_block_into(
            &cluster.bytes,
            cluster.cluster_id,
            block.score_block,
            &mut self.entries,
        )?;
        self.block_index = block_index;
        self.position_in_block = 0;
        Ok(())
    }

    fn exhausted(&self) -> bool {
        self.block_index >= self.blocks.len()
    }
}

impl PostingCursor for ClusteredPostingCursor {
    fn doc_freq(&self) -> u64 {
        self.doc_freq
    }

    fn ordinal(&self) -> u64 {
        if self.exhausted() {
            return self.doc_freq;
        }
        self.blocks[self.block_index]
            .first_ordinal
            .saturating_add(self.position_in_block as u64)
    }

    fn current(&self) -> Option<PostingScore> {
        self.entries.get(self.position_in_block).copied()
    }

    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        if self.exhausted() {
            return Ok(None);
        }
        self.position_in_block += 1;
        if self.position_in_block < self.entries.len() {
            return Ok(self.current());
        }
        self.load_block(self.block_index + 1)?;
        Ok(self.current())
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        if self.current().is_some_and(|entry| entry.doc_id >= target) {
            return Ok(self.current());
        }
        if self.exhausted() {
            return Ok(None);
        }
        let relative =
            self.blocks[self.block_index..].partition_point(|block| block.last_doc_id < target);
        let block_index = self.block_index + relative;
        if block_index >= self.blocks.len() {
            self.load_block(self.blocks.len())?;
            return Ok(None);
        }
        if block_index != self.block_index {
            self.load_block(block_index)?;
        }
        self.position_in_block = self.entries.partition_point(|entry| entry.doc_id < target);
        if self.position_in_block < self.entries.len() {
            return Ok(self.current());
        }
        self.load_block(block_index + 1)?;
        Ok(self.current())
    }

    fn boxed_clone(&self) -> Box<dyn PostingCursor> {
        Box::new(self.clone())
    }
}

pub(crate) fn cluster_id(doc_id: DocId) -> u64 {
    doc_id / POSTING_CLUSTER_DOCS
}

fn cluster_base(cluster_id: u64) -> StorageBackendResult<DocId> {
    cluster_id
        .checked_mul(POSTING_CLUSTER_DOCS)
        .ok_or_else(|| corrupt("cluster base document id overflow"))
}

fn cluster_offset(doc_id: DocId) -> StorageBackendResult<u16> {
    u16::try_from(doc_id % POSTING_CLUSTER_DOCS)
        .map_err(|_| corrupt("document offset exceeds clustered format"))
}

pub(crate) fn encode_cluster(
    entries: &[ClusterPosting],
) -> StorageBackendResult<(Vec<u8>, Vec<u8>)> {
    if entries.is_empty() {
        return Err(corrupt("cannot encode an empty posting cluster"));
    }
    validate_cluster_entries(entries)?;
    Ok((encode_scores(entries)?, encode_positions(entries)?))
}

pub(crate) fn decode_cluster(
    cluster_id: u64,
    score_blob: &[u8],
    positions_blob: &[u8],
) -> StorageBackendResult<Vec<ClusterPosting>> {
    let scores = decode_all_scores(cluster_id, score_blob)?;
    let positions = decode_positions(positions_blob, &scores)?;
    Ok(scores
        .into_iter()
        .zip(positions)
        .map(|(score, positions)| ClusterPosting {
            doc_id: score.doc_id,
            term_freq: score.term_freq,
            doc_length: score.doc_length,
            positions,
        })
        .collect())
}

pub(crate) fn score_count(score_blob: &[u8]) -> StorageBackendResult<u64> {
    let (count, _) = parse_score_blob(score_blob)?;
    Ok(count as u64)
}

pub(crate) fn encode_terms(terms: &[String]) -> StorageBackendResult<Vec<u8>> {
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

pub(crate) fn decode_terms(blob: &[u8]) -> StorageBackendResult<Vec<String>> {
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

pub(crate) fn decode_all_scores(
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

fn encode_scores(entries: &[ClusterPosting]) -> StorageBackendResult<Vec<u8>> {
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
    output.push(FORMAT_VERSION);
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

fn encode_positions(entries: &[ClusterPosting]) -> StorageBackendResult<Vec<u8>> {
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

fn parse_score_blob(blob: &[u8]) -> StorageBackendResult<(usize, Vec<ScoreBlock>)> {
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

fn decode_score_block_into(
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
        if term_freq == 0 || doc_length < term_freq {
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

fn decode_positions(blob: &[u8], scores: &[PostingScore]) -> StorageBackendResult<Vec<Vec<u32>>> {
    validate_header(blob, *POSITIONS_MAGIC)?;
    let count = read_u32(blob, 8)? as usize;
    let offset_count = read_u32(blob, 12)? as usize;
    if count != scores.len() || offset_count != count.saturating_add(1) {
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

    let mut all = Vec::with_capacity(count);
    for (index, score) in scores.iter().enumerate() {
        let mut encoded = &data[offsets[index]..offsets[index + 1]];
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

fn validate_cluster_entries(entries: &[ClusterPosting]) -> StorageBackendResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let expected_cluster = cluster_id(entries[0].doc_id);
    let mut previous = None;
    for entry in entries {
        if cluster_id(entry.doc_id) != expected_cluster {
            return Err(corrupt("one encoded value spans multiple clusters"));
        }
        if previous.is_some_and(|doc_id| doc_id >= entry.doc_id) {
            return Err(corrupt("posting document ids are not strictly increasing"));
        }
        if entry.term_freq == 0
            || entry.doc_length < entry.term_freq
            || entry.term_freq != entry.positions.len() as u64
        {
            return Err(corrupt(
                "posting frequency, document length, and positions disagree",
            ));
        }
        if entry.positions.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(corrupt("term positions are not strictly increasing"));
        }
        previous = Some(entry.doc_id);
    }
    Ok(())
}

fn validate_scores(entries: &[PostingScore]) -> StorageBackendResult<()> {
    let mut previous = None;
    for entry in entries {
        if previous.is_some_and(|doc_id| doc_id >= entry.doc_id) {
            return Err(corrupt("posting scores are not strictly ordered"));
        }
        if entry.term_freq == 0 || entry.doc_length < entry.term_freq {
            return Err(corrupt("invalid posting score frequencies"));
        }
        previous = Some(entry.doc_id);
    }
    Ok(())
}

fn validate_header(blob: &[u8], magic: [u8; 4]) -> StorageBackendResult<()> {
    if blob.len() < HEADER_LEN || blob.get(..4) != Some(magic.as_slice()) {
        return Err(corrupt("missing clustered posting header"));
    }
    if blob[4] != FORMAT_VERSION {
        return Err(corrupt("unsupported clustered posting version"));
    }
    if blob[5..8] != [0; 3] {
        return Err(corrupt("clustered posting reserved header bits are set"));
    }
    Ok(())
}

fn put_varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn read_varint(input: &mut &[u8]) -> StorageBackendResult<u64> {
    let mut value = 0_u64;
    for shift in (0..=63).step_by(7) {
        let Some((&byte, rest)) = input.split_first() else {
            return Err(corrupt("truncated varint"));
        };
        *input = rest;
        if shift == 63 && byte > 1 {
            return Err(corrupt("varint overflow"));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(corrupt("unterminated varint"))
}

fn put_u32(output: &mut Vec<u8>, value: usize, field: &str) -> StorageBackendResult<()> {
    let value = u32::try_from(value)
        .map_err(|_| corrupt(format!("{field} exceeds the u32 on-disk format")))?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_u16(input: &[u8], offset: usize) -> StorageBackendResult<u16> {
    let bytes: [u8; 2] = input
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| corrupt("truncated u16 field"))?
        .try_into()
        .map_err(|_| corrupt("invalid u16 field"))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &[u8], offset: usize) -> StorageBackendResult<u32> {
    let bytes: [u8; 4] = input
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| corrupt("truncated u32 field"))?
        .try_into()
        .map_err(|_| corrupt("invalid u32 field"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt clustered posting: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posting(doc_id: DocId, positions: &[u32], doc_length: u64) -> ClusterPosting {
        ClusterPosting {
            doc_id,
            term_freq: positions.len() as u64,
            doc_length,
            positions: positions.to_vec(),
        }
    }

    #[test]
    fn clustered_round_trip_separates_scores_and_positions() {
        let entries = vec![
            posting(2, &[1, 4], 8),
            posting(9, &[0], 3),
            posting(65_535, &[2, 7, 11], 20),
        ];
        let (scores, positions) = encode_cluster(&entries).unwrap();
        assert_eq!(score_count(&scores).unwrap(), 3);
        assert_eq!(decode_all_scores(0, &scores).unwrap()[0].term_freq, 2);
        assert_eq!(decode_cluster(0, &scores, &positions).unwrap(), entries);
    }

    #[test]
    fn lazy_cursor_decodes_across_blocks_and_clusters() {
        let first = (0..260_u64)
            .map(|doc_id| posting(doc_id * 2, &[0], 4))
            .collect::<Vec<_>>();
        let second = vec![posting(POSTING_CLUSTER_DOCS + 7, &[1, 3], 9)];
        let (first_scores, _) = encode_cluster(&first).unwrap();
        let (second_scores, _) = encode_cluster(&second).unwrap();
        let mut cursor = ClusteredPostingCursor::new(vec![
            EncodedScoreCluster {
                cluster_id: 0,
                bytes: first_scores,
            },
            EncodedScoreCluster {
                cluster_id: 1,
                bytes: second_scores,
            },
        ])
        .unwrap();
        assert_eq!(cursor.doc_freq(), 261);
        assert_eq!(cursor.current().unwrap().doc_id, 0);
        assert_eq!(cursor.advance_to(400).unwrap().unwrap().doc_id, 400);
        assert_eq!(cursor.ordinal(), 200);
        assert_eq!(
            cursor
                .advance_to(POSTING_CLUSTER_DOCS + 1)
                .unwrap()
                .unwrap(),
            PostingScore {
                doc_id: POSTING_CLUSTER_DOCS + 7,
                term_freq: 2,
                doc_length: 9,
            }
        );
        assert!(cursor.advance().unwrap().is_none());
    }

    #[test]
    fn malformed_cluster_is_rejected_before_iteration() {
        let entries = vec![posting(1, &[0], 1)];
        let (mut scores, positions) = encode_cluster(&entries).unwrap();
        scores[4] = 99;
        assert!(decode_cluster(0, &scores, &positions).is_err());
        assert!(ClusteredPostingCursor::new(vec![EncodedScoreCluster {
            cluster_id: 0,
            bytes: scores,
        }])
        .is_err());
    }

    #[test]
    fn cursor_rejects_overlapping_block_ranges_before_iteration() {
        let entries = (0..130_u64)
            .map(|doc_id| posting(doc_id, &[0], 1))
            .collect::<Vec<_>>();
        let (mut scores, _) = encode_cluster(&entries).unwrap();
        let second_directory = HEADER_LEN + SCORE_DIRECTORY_ENTRY_LEN;
        let second_docs_start = read_u32(&scores, second_directory + 4).unwrap() as usize;
        scores[second_docs_start] = 0;
        assert!(ClusteredPostingCursor::new(vec![EncodedScoreCluster {
            cluster_id: 0,
            bytes: scores,
        }])
        .is_err());
    }

    #[test]
    fn empty_cluster_has_no_persisted_representation() {
        assert!(encode_cluster(&[]).is_err());
    }

    #[test]
    fn document_terms_round_trip() {
        let terms = vec!["alpha".to_string(), "rust".to_string(), "검색".to_string()];
        assert_eq!(decode_terms(&encode_terms(&terms).unwrap()).unwrap(), terms);
        assert!(encode_terms(&["same".into(), "same".into()]).is_err());
        let mut oversized_count = encode_terms(&[]).unwrap();
        oversized_count[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_terms(&oversized_count).is_err());
    }
}
