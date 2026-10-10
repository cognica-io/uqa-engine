//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode bounded score blocks as postings arrive, retaining only their encoded streams.

use uqa_core::memory::BudgetedVec;

use super::super::encoding::{put_u32, put_varint};
use super::super::{
    cluster_offset, corrupt, PostingScore, StorageBackendResult, DEFAULT_BLOCK_SIZE, HEADER_LEN,
    SCORE_DIRECTORY_ENTRY_LEN, SCORE_MAGIC,
};
use crate::read_control::StorageReadControl;

struct EncodedBlock {
    count: u16,
    last_offset: u16,
    docs: BudgetedVec<u8>,
    term_freqs: BudgetedVec<u8>,
    doc_lengths: BudgetedVec<u8>,
}

/// One unfinished score block plus completed encoded streams; growing a cluster never relocates a cluster-sized array of decoded postings.
pub(in crate::clustered_postings) struct ScoreEncoder {
    blocks: BudgetedVec<EncodedBlock>,
    pending: BudgetedVec<PostingScore>,
    count: usize,
    previous: Option<u64>,
    invalid: Option<&'static str>,
    control: StorageReadControl,
}

impl ScoreEncoder {
    pub(in crate::clustered_postings) fn new(control: &StorageReadControl) -> Self {
        Self {
            blocks: BudgetedVec::new(control.memory()),
            pending: BudgetedVec::new(control.memory()),
            count: 0,
            previous: None,
            invalid: None,
            control: control.clone(),
        }
    }

    pub(in crate::clustered_postings) fn len(&self) -> usize {
        self.count
    }
    pub(in crate::clustered_postings) fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(in crate::clustered_postings) fn reserve(
        &mut self,
        count: usize,
    ) -> StorageBackendResult<()> {
        self.blocks.reserve(count.div_ceil(DEFAULT_BLOCK_SIZE))?;
        self.pending.reserve(
            count
                .min(DEFAULT_BLOCK_SIZE)
                .saturating_sub(self.pending.len()),
        )?;
        Ok(())
    }

    pub(in crate::clustered_postings) fn push(
        &mut self,
        entry: PostingScore,
    ) -> StorageBackendResult<()> {
        self.control.cancellation().check()?;
        if self.invalid.is_none() {
            self.invalid = if self
                .previous
                .is_some_and(|previous| previous >= entry.doc_id)
            {
                Some("posting scores are not strictly ordered")
            } else if entry.term_freq == 0 || entry.doc_length == 0 {
                Some("invalid posting score frequencies")
            } else {
                None
            };
        }
        self.previous = Some(entry.doc_id);
        if self.pending.len() == DEFAULT_BLOCK_SIZE {
            self.seal_block()?;
        }
        self.pending.push(entry)?;
        self.count += 1;
        Ok(())
    }

    fn seal_block(&mut self) -> StorageBackendResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if let Some(message) = self.invalid {
            return Err(corrupt(message));
        }
        let chunk = &self.pending;
        let mut docs = BudgetedVec::new(self.control.memory());
        let mut term_freqs = BudgetedVec::new(self.control.memory());
        let mut doc_lengths = BudgetedVec::new(self.control.memory());
        // Every encoded value occupies at least one byte. Reserve that known stream size before appending variable-width values.
        docs.reserve(chunk.len())?;
        term_freqs.reserve(chunk.len())?;
        doc_lengths.reserve(chunk.len())?;
        let mut previous = 0_u16;
        for (index, entry) in chunk.iter().enumerate() {
            self.control.cancellation().check()?;
            let offset = cluster_offset(entry.doc_id)?;
            let delta = if index == 0 {
                u64::from(offset)
            } else {
                u64::from(offset.checked_sub(previous).ok_or_else(|| {
                    corrupt("posting document offsets are not strictly increasing")
                })?)
            };
            put_varint(&mut docs, delta)?;
            put_varint(&mut term_freqs, entry.term_freq)?;
            put_varint(&mut doc_lengths, entry.doc_length)?;
            previous = offset;
        }
        self.blocks.push(EncodedBlock {
            count: u16::try_from(chunk.len())
                .map_err(|_| corrupt("score block contains too many postings"))?,
            last_offset: cluster_offset(chunk.last().expect("non-empty score block").doc_id)?,
            docs,
            term_freqs,
            doc_lengths,
        })?;
        self.pending.clear();
        Ok(())
    }

    pub(in crate::clustered_postings) fn finish(
        mut self,
        version: u8,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.control = control.clone();
        self.control.cancellation().check()?;
        if let Some(message) = self.invalid {
            return Err(corrupt(message));
        }
        self.seal_block()?;
        drop(self.pending);
        let directory_bytes = self
            .blocks
            .len()
            .checked_mul(SCORE_DIRECTORY_ENTRY_LEN)
            .ok_or_else(|| corrupt("score directory size overflow"))?;
        let data_start = HEADER_LEN
            .checked_add(directory_bytes)
            .ok_or_else(|| corrupt("score blob size overflow"))?;
        let data_bytes = self.blocks.iter().try_fold(0_usize, |total, block| {
            total
                .checked_add(block.docs.len())
                .and_then(|value| value.checked_add(block.term_freqs.len()))
                .and_then(|value| value.checked_add(block.doc_lengths.len()))
                .ok_or_else(|| corrupt("score blob size overflow"))
        })?;
        let mut output = BudgetedVec::new(self.control.memory());
        output.reserve(
            data_start
                .checked_add(data_bytes)
                .ok_or_else(|| corrupt("score blob size overflow"))?,
        )?;
        output.extend_from_slice(SCORE_MAGIC)?;
        output.push(version)?;
        output.extend_from_slice(&[0; 3])?;
        put_u32(&mut output, self.count, "posting count")?;
        put_u32(&mut output, self.blocks.len(), "score block count")?;

        let mut offset = data_start;
        for block in self.blocks.iter() {
            output.extend_from_slice(&block.count.to_le_bytes())?;
            output.extend_from_slice(&block.last_offset.to_le_bytes())?;
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
        for block in self.blocks.iter() {
            output.extend_from_slice(&block.docs)?;
            output.extend_from_slice(&block.term_freqs)?;
            output.extend_from_slice(&block.doc_lengths)?;
        }
        Ok(output)
    }
}
