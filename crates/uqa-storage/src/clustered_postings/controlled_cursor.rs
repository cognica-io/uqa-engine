//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain one encoded cluster and reuse one score block under the consumer's allocation owner.

use super::{
    cluster_base, cluster_id, corrupt,
    scores::{visit_score_block, ScoreDirectory},
    BudgetedPostingReadCursor, DocId, PostingReadCursor, PostingScore, ScoreBlock,
    StorageBackendResult, DEFAULT_BLOCK_SIZE, OCCURRENCE_FORMAT_VERSION,
};
use crate::{read_control::StorageReadControl, InvertedIndex, TokenTermKey};
use uqa_core::memory::BudgetedVec;

#[derive(Clone, Copy)]
pub struct EncodedScoreClusterRef<'a> {
    pub cluster_id: u64,
    pub stored_count: Option<u64>,
    pub bytes: &'a [u8],
}
pub type ScoreClusterVisitor<'a> =
    dyn FnMut(EncodedScoreClusterRef<'_>) -> StorageBackendResult<()> + 'a;

struct EncodedCluster {
    id: u64,
    bytes: BudgetedVec<u8>,
    blocks: BudgetedVec<ScoreBlock>,
}

fn directory<'a>(
    cluster: &EncodedScoreClusterRef<'a>,
    control: &StorageReadControl,
) -> StorageBackendResult<ScoreDirectory<'a>> {
    control.check()?;
    cluster_base(cluster.cluster_id)?;
    let directory = ScoreDirectory::new(cluster.bytes, &mut || control.check())?;
    if cluster.bytes[4] != OCCURRENCE_FORMAT_VERSION {
        return Err(corrupt("occurrence index contains a legacy score payload"));
    }
    if cluster
        .stored_count
        .is_some_and(|count| count != directory.count as u64)
    {
        return Err(corrupt(
            "stored posting count disagrees with the score payload",
        ));
    }
    Ok(directory)
}

fn retain(
    cluster: EncodedScoreClusterRef<'_>,
    directory: &ScoreDirectory<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<EncodedCluster> {
    let mut bytes = BudgetedVec::new(control.memory());
    bytes.reserve(cluster.bytes.len())?;
    for (index, byte) in cluster.bytes.iter().copied().enumerate() {
        if index % 1024 == 0 {
            control.check()?;
        }
        bytes.push(byte)?;
    }
    let mut blocks = BudgetedVec::new(control.memory());
    blocks.reserve(directory.blocks)?;
    for index in 0..directory.blocks {
        control.check()?;
        blocks.push(directory.block(index)?)?;
    }
    control.check()?;
    Ok(EncodedCluster {
        id: cluster.cluster_id,
        bytes,
        blocks,
    })
}

struct ControlledCursor<'a, T: InvertedIndex + ?Sized> {
    index: &'a T,
    field: &'a str,
    term: &'a TokenTermKey,
    control: StorageReadControl,
    cluster: Option<EncodedCluster>,
    entries: BudgetedVec<PostingScore>,
    doc_freq: u64,
    block: usize,
    position: usize,
}

pub(crate) fn open<'a, T: InvertedIndex + ?Sized>(
    index: &'a T,
    field: &'a str,
    term: &'a TokenTermKey,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedPostingReadCursor<'a>> {
    let mut first = None;
    let mut previous = None;
    let mut doc_freq = 0u64;
    index.visit_score_clusters(field, term, None, usize::MAX, control, &mut |cluster| {
        control.check()?;
        if previous.is_some_and(|previous| previous >= cluster.cluster_id) {
            return Err(corrupt("cluster identifiers are not strictly increasing"));
        }
        previous = Some(cluster.cluster_id);
        let directory = directory(&cluster, control)?;
        doc_freq = doc_freq
            .checked_add(directory.count as u64)
            .ok_or_else(|| corrupt("posting count overflow"))?;
        if first.is_none() {
            first = Some(retain(cluster, &directory, control)?);
        }
        Ok(())
    })?;
    control.check()?;
    let mut cursor = ControlledCursor {
        index,
        field,
        term,
        control: control.clone(),
        cluster: first,
        entries: BudgetedVec::new(control.memory()),
        doc_freq,
        block: 0,
        position: 0,
    };
    if let Some(cluster) = &cursor.cluster {
        decode_block(cluster, 0, &mut cursor.entries, control)?;
    }
    BudgetedPostingReadCursor::new(cursor, control)
}

fn decode_block(
    cluster: &EncodedCluster,
    at: usize,
    entries: &mut BudgetedVec<PostingScore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    // A fixed block staging area preserves the current cursor on cancellation or corrupt input.
    let mut pending = [PostingScore {
        doc_id: 0,
        term_freq: 0,
        doc_length: 0,
    }; DEFAULT_BLOCK_SIZE];
    let block = cluster.blocks[at];
    let mut count = 0;
    visit_score_block(
        &cluster.bytes,
        cluster.id,
        block,
        &mut || control.check(),
        |entry, _| {
            pending[count] = entry;
            count += 1;
            Ok(())
        },
    )?;
    control.check()?;
    entries.reserve(count.saturating_sub(entries.len()))?;
    entries.clear();
    for entry in &pending[..count] {
        entries.push(*entry)?;
    }
    Ok(())
}

impl<T: InvertedIndex + ?Sized> ControlledCursor<'_, T> {
    fn load_after(&mut self, after: u64) -> StorageBackendResult<bool> {
        let mut next = None;
        self.index.visit_score_clusters(
            self.field,
            self.term,
            Some(after),
            1,
            &self.control,
            &mut |cluster| {
                if cluster.cluster_id <= after || next.is_some() {
                    return Err(corrupt("score source violated the requested cluster range"));
                }
                let directory = directory(&cluster, &self.control)?;
                next = Some(retain(cluster, &directory, &self.control)?);
                Ok(())
            },
        )?;
        self.control.check()?;
        if let Some(cluster) = &next {
            decode_block(cluster, 0, &mut self.entries, &self.control)?;
        } else {
            self.entries = BudgetedVec::new(self.control.memory());
        }
        self.cluster = next;
        self.block = 0;
        self.position = 0;
        Ok(self.cluster.is_some())
    }

    fn load_block(&mut self, at: usize) -> StorageBackendResult<()> {
        let cluster = self.cluster.as_ref().expect("live cluster");
        decode_block(cluster, at, &mut self.entries, &self.control)?;
        self.block = at;
        self.position = 0;
        Ok(())
    }
}

impl<T: InvertedIndex + ?Sized> PostingReadCursor for ControlledCursor<'_, T> {
    fn doc_freq(&self) -> u64 {
        self.doc_freq
    }
    fn current(&self) -> Option<PostingScore> {
        self.entries.get(self.position).copied()
    }
    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        self.control.check()?;
        let Some(cluster) = &self.cluster else {
            return Ok(None);
        };
        if self.position + 1 < self.entries.len() {
            self.position += 1;
        } else if self.block + 1 < cluster.blocks.len() {
            self.load_block(self.block + 1)?;
        } else {
            self.load_after(cluster.id)?;
        }
        Ok(self.current())
    }
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        self.control.check()?;
        if self.current().is_none_or(|entry| entry.doc_id >= target) {
            return Ok(self.current());
        }
        loop {
            self.control.check()?;
            let Some(cluster) = &self.cluster else {
                return Ok(None);
            };
            let base = cluster_base(cluster.id)?;
            let at = cluster
                .blocks
                .partition_point(|block| base + u64::from(block.last_offset) < target);
            if at < cluster.blocks.len() {
                if at != self.block {
                    self.load_block(at)?;
                }
                self.position = self.entries.partition_point(|entry| entry.doc_id < target);
                return Ok(self.current());
            }
            let after = cluster.id.max(cluster_id(target).saturating_sub(1));
            if !self.load_after(after)? {
                return Ok(None);
            }
        }
    }
}
