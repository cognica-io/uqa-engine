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

use uqa_core::{DocId, TokenOccurrence};

use crate::{StorageBackendError, StorageBackendResult, DEFAULT_BLOCK_SIZE};

pub const POSTING_CLUSTER_DOCS: u64 = 1 << 16;

const SCORE_MAGIC: &[u8; 4] = b"UQCS";
const POSITIONS_MAGIC: &[u8; 4] = b"UQCP";
const TERMS_MAGIC: &[u8; 4] = b"UQCT";
const FORMAT_VERSION: u8 = 1;
pub const OCCURRENCE_FORMAT_VERSION: u8 = 2;
const HEADER_LEN: usize = 16;
const SCORE_DIRECTORY_ENTRY_LEN: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingScore {
    pub doc_id: DocId,
    pub term_freq: u64,
    pub doc_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterPosting {
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
pub struct EncodedScoreCluster {
    pub cluster_id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct ScoreBlock {
    version: u8,
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
pub struct ClusteredPostingCursor {
    clusters: Arc<[EncodedScoreCluster]>,
    blocks: Arc<[CursorBlock]>,
    doc_freq: u64,
    block_index: usize,
    entries: Vec<PostingScore>,
    position_in_block: usize,
}

impl ClusteredPostingCursor {
    pub fn new(clusters: Vec<EncodedScoreCluster>) -> StorageBackendResult<Self> {
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

pub fn cluster_id(doc_id: DocId) -> u64 {
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

pub fn encode_cluster(entries: &[ClusterPosting]) -> StorageBackendResult<(Vec<u8>, Vec<u8>)> {
    if entries.is_empty() {
        return Err(corrupt("cannot encode an empty posting cluster"));
    }
    validate_cluster_entries(entries)?;
    let scores: Vec<_> = entries
        .iter()
        .map(|entry| PostingScore {
            doc_id: entry.doc_id,
            term_freq: entry.term_freq,
            doc_length: entry.doc_length,
        })
        .collect();
    Ok((
        encode_scores(&scores, FORMAT_VERSION)?,
        encode_positions(entries)?,
    ))
}

pub fn decode_cluster(
    cluster_id: u64,
    score_blob: &[u8],
    positions_blob: &[u8],
) -> StorageBackendResult<Vec<ClusterPosting>> {
    if score_blob.get(4) != Some(&FORMAT_VERSION) {
        return Err(corrupt("legacy cluster reader requires format version 1"));
    }
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

pub fn score_count(score_blob: &[u8]) -> StorageBackendResult<u64> {
    let (count, _) = parse_score_blob(score_blob)?;
    Ok(count as u64)
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
        if entry.term_freq == 0 || entry.doc_length == 0 {
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
    if ![FORMAT_VERSION, OCCURRENCE_FORMAT_VERSION].contains(&blob[4]) {
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
            if shift > 0 && byte == 0 {
                return Err(corrupt("noncanonical varint"));
            }
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

mod legacy;
mod occurrences;
mod scores;
mod term_keys;

use legacy::{decode_positions, encode_positions};
pub use legacy::{decode_terms, encode_terms};
pub use occurrences::{decode_occurrence_cluster, encode_occurrence_cluster, OccurrencePosting};
use scores::{decode_score_block_into, encode_scores, parse_score_blob};
pub use term_keys::{decode_term_keys, encode_term_keys};

#[cfg(test)]
mod tests;
