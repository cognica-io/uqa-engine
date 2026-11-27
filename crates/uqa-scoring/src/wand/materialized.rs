//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized posting-list cursor state and exact WAND/BMW pivot loops.

use std::sync::Arc;

use uqa_core::{DocId, FieldName, PostingEntry, PostingList};
use uqa_storage::{BlockMaxIndex, InvertedIndex, StorageBackendResult};

use crate::scorer::Scorer;
use uqa_storage::TokenTermKey;

use super::common::{
    invalid_wand_input, run_pivot_loop, validate_shape, BlockBounds, WANDResult, WandCursorSource,
    WandSelection, WandTerm,
};

struct TermCursor<'a> {
    entries: &'a [PostingEntry],
    position: usize,
    index: Option<&'a dyn InvertedIndex>,
    field: &'a str,
    term: &'a TokenTermKey,
    field_slot: usize,
}

impl WandCursorSource for TermCursor<'_> {
    const NAME: &'static str = "WAND";
    const COLLECTION: &'static str = "posting_lists";

    fn current_doc(&self) -> Option<DocId> {
        self.entries.get(self.position).map(|entry| entry.doc_id)
    }

    fn ordinal(&self) -> StorageBackendResult<usize> {
        Ok(self.position)
    }

    fn advance(&mut self) -> StorageBackendResult<()> {
        self.position += 1;
        Ok(())
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<()> {
        self.position +=
            self.entries[self.position..].partition_point(|entry| entry.doc_id < target);
        Ok(())
    }

    fn term_inputs(
        &self,
        doc: DocId,
        lengths: &mut [Option<u64>],
    ) -> StorageBackendResult<Option<(u64, u64)>> {
        let Some(entry) = self
            .entries
            .get(self.position)
            .filter(|entry| entry.doc_id == doc)
        else {
            return Ok(None);
        };
        if let Some(index) = self.index {
            let tf = index.get_term_freq_key(doc, self.field, self.term)?;
            let length = if let Some(length) = lengths[self.field_slot] {
                length
            } else {
                let length = index.get_doc_length(doc, self.field)?;
                lengths[self.field_slot] = Some(length);
                length
            };
            Ok(Some((tf, length)))
        } else {
            let tf = u64::try_from(entry.payload.positions.len().max(1))
                .map_err(|_| invalid_wand_input("term frequency does not fit in u64"))?;
            Ok(Some((tf, tf)))
        }
    }
}

/// WAND configuration shared by both algorithms.
pub struct WANDQuery {
    pub posting_lists: Vec<PostingList>,
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub fields: Vec<FieldName>,
    pub terms: Vec<TokenTermKey>,
    pub k: usize,
}

impl WANDQuery {
    pub fn new(
        posting_lists: Vec<PostingList>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<String>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        Self::new_keys(
            posting_lists,
            scorers,
            fields,
            terms.into_iter().map(TokenTermKey::from).collect(),
            k,
        )
    }
}

impl WANDQuery {
    pub fn new_keys(
        posting_lists: Vec<PostingList>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<TokenTermKey>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        validate_shape::<TermCursor<'_>>(
            posting_lists.len(),
            scorers.len(),
            fields.len(),
            terms.len(),
        )?;
        Ok(Self {
            posting_lists,
            scorers,
            fields,
            terms,
            k,
        })
    }
}

impl WANDQuery {
    /// Count distinct candidate documents for diagnostics. This reads the
    /// posting lists in full and is independent of top-k pruning.
    pub fn exact_candidate_count(&self) -> StorageBackendResult<u64> {
        candidate_union(&self.posting_lists)
    }

    fn select(
        &self,
        index: Option<&dyn InvertedIndex>,
        blocks: Option<(&BlockMaxIndex, &str)>,
    ) -> StorageBackendResult<WandSelection> {
        validate_shape::<TermCursor<'_>>(
            self.posting_lists.len(),
            self.scorers.len(),
            self.fields.len(),
            self.terms.len(),
        )?;
        let mut slots = std::collections::HashMap::new();
        let mut terms = Vec::with_capacity(self.posting_lists.len());
        for (i, list) in self.posting_lists.iter().enumerate() {
            let df = u64::try_from(list.len())
                .map_err(|_| invalid_wand_input("posting-list length does not fit in u64"))?;
            let next_slot = slots.len();
            let field_slot = *slots.entry(self.fields[i].as_str()).or_insert(next_slot);
            let cursor = TermCursor {
                entries: list.entries(),
                position: 0,
                index,
                field: &self.fields[i],
                term: &self.terms[i],
                field_slot,
            };
            terms.push(WandTerm::new(cursor, df, self.scorers[i].as_ref())?);
        }
        let bounds = blocks.map(|(b, table)| BlockBounds::new(b, table, &self.fields, &self.terms));
        run_pivot_loop(
            &mut terms,
            &self.scorers,
            self.k,
            slots.len(),
            bounds.as_ref(),
        )
    }
}

/// Standard WAND with per-term `term_upper_bound(df)` pruning.
pub struct WANDScorer<'a> {
    query: &'a WANDQuery,
    inverted_index: Option<&'a dyn InvertedIndex>,
}

impl<'a> WANDScorer<'a> {
    pub fn new(query: &'a WANDQuery, inverted_index: Option<&'a dyn InvertedIndex>) -> Self {
        Self {
            query,
            inverted_index,
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        self.query
            .select(self.inverted_index, None)
            .map(WandSelection::into_postings)
    }
}

/// Block-Max WAND: pivot pruning uses per-block max scores from
/// [`BlockMaxIndex`] for tighter bounds.
pub struct BlockMaxWANDScorer<'a> {
    query: &'a WANDQuery,
    inverted_index: Option<&'a dyn InvertedIndex>,
    block_max_index: &'a BlockMaxIndex,
    table: String,
}

impl<'a> BlockMaxWANDScorer<'a> {
    pub fn new(
        query: &'a WANDQuery,
        inverted_index: Option<&'a dyn InvertedIndex>,
        block_max_index: &'a BlockMaxIndex,
        table: impl Into<String>,
    ) -> Self {
        Self {
            query,
            inverted_index,
            block_max_index,
            table: table.into(),
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        self.query
            .select(
                self.inverted_index,
                Some((self.block_max_index, &self.table)),
            )
            .map(WandSelection::into_postings)
    }
}

pub(super) fn candidate_union(posting_lists: &[PostingList]) -> StorageBackendResult<u64> {
    super::common::count_distinct_docs(posting_lists.iter().map(PostingList::doc_ids))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or_else(|| invalid_wand_input("candidate union length does not fit in u64"))
}
