//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Score-only posting cursor state and exact WAND/BMW pivot loops.

use std::sync::Arc;

use uqa_core::{DocId, FieldName, ScoredEntry};
use uqa_storage::{BlockMaxIndex, PostingCursor, StorageBackendResult};

use crate::scorer::Scorer;
use uqa_storage::TokenTermKey;

use super::common::{
    invalid_wand_input, run_pivot_loop, validate_shape, BlockBounds, WANDResult, WANDStats,
    WandCursorSource, WandSelection, WandTerm,
};

/// WAND query backed directly by score-only posting cursors.
///
/// Unlike [`super::materialized::WANDQuery`], this form never materializes positional payloads and
/// carries document length beside term frequency in each cursor entry.
pub struct CursorWANDQuery {
    pub cursors: Vec<Box<dyn PostingCursor>>,
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub fields: Vec<FieldName>,
    pub terms: Vec<TokenTermKey>,
    pub k: usize,
}

impl CursorWANDQuery {
    pub fn new(
        cursors: Vec<Box<dyn PostingCursor>>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<String>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        Self::new_keys(
            cursors,
            scorers,
            fields,
            terms.into_iter().map(TokenTermKey::from).collect(),
            k,
        )
    }
}

impl CursorWANDQuery {
    pub fn new_keys(
        cursors: Vec<Box<dyn PostingCursor>>,
        scorers: Vec<Arc<dyn Scorer>>,
        fields: Vec<FieldName>,
        terms: Vec<TokenTermKey>,
        k: usize,
    ) -> StorageBackendResult<Self> {
        validate_shape::<Box<dyn PostingCursor>>(
            cursors.len(),
            scorers.len(),
            fields.len(),
            terms.len(),
        )?;
        Ok(Self {
            cursors,
            scorers,
            fields,
            terms,
            k,
        })
    }
}

impl WandCursorSource for Box<dyn PostingCursor> {
    const NAME: &'static str = "cursor WAND";
    const COLLECTION: &'static str = "cursors";

    fn current_doc(&self) -> Option<DocId> {
        self.current().map(|entry| entry.doc_id)
    }

    fn ordinal(&self) -> StorageBackendResult<usize> {
        usize::try_from(self.as_ref().ordinal())
            .map_err(|_| invalid_wand_input("posting cursor ordinal does not fit in usize"))
    }

    fn advance(&mut self) -> StorageBackendResult<()> {
        self.as_mut().advance().map(drop)
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<()> {
        self.as_mut().advance_to(target).map(drop)
    }

    fn term_inputs(
        &self,
        doc: DocId,
        _lengths: &mut [Option<u64>],
    ) -> StorageBackendResult<Option<(u64, u64)>> {
        Ok(self
            .current()
            .filter(|entry| entry.doc_id == doc)
            .map(|entry| (entry.term_freq, entry.doc_length)))
    }
}

impl CursorWANDQuery {
    fn select(
        &self,
        blocks: Option<(&BlockMaxIndex, &str)>,
    ) -> StorageBackendResult<WandSelection> {
        validate_shape::<Box<dyn PostingCursor>>(
            self.cursors.len(),
            self.scorers.len(),
            self.fields.len(),
            self.terms.len(),
        )?;
        let mut terms = self
            .cursors
            .iter()
            .zip(&self.scorers)
            .map(|(cursor, scorer)| {
                WandTerm::new(cursor.clone(), cursor.doc_freq(), scorer.as_ref())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let bounds = blocks.map(|(b, table)| BlockBounds::new(b, table, &self.fields, &self.terms));
        run_pivot_loop(&mut terms, &self.scorers, self.k, 0, bounds.as_ref())
    }

    pub(crate) fn score_ranked(
        &self,
        blocks: Option<(&BlockMaxIndex, &str)>,
    ) -> StorageBackendResult<(Vec<ScoredEntry>, WANDStats)> {
        self.select(blocks).map(WandSelection::into_ranked)
    }
}

/// Standard WAND over score-only posting cursors.
pub struct CursorWANDScorer<'a> {
    query: &'a CursorWANDQuery,
}

impl<'a> CursorWANDScorer<'a> {
    pub fn new(query: &'a CursorWANDQuery) -> Self {
        Self { query }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        self.query.select(None).map(WandSelection::into_postings)
    }
}

/// Block-Max WAND over score-only posting cursors.
pub struct CursorBlockMaxWANDScorer<'a> {
    query: &'a CursorWANDQuery,
    block_max_index: &'a BlockMaxIndex,
    table: String,
}

impl<'a> CursorBlockMaxWANDScorer<'a> {
    pub fn new(
        query: &'a CursorWANDQuery,
        block_max_index: &'a BlockMaxIndex,
        table: impl Into<String>,
    ) -> Self {
        Self {
            query,
            block_max_index,
            table: table.into(),
        }
    }

    pub fn score_top_k(&self) -> StorageBackendResult<WANDResult> {
        self.query
            .select(Some((self.block_max_index, &self.table)))
            .map(WandSelection::into_postings)
    }
}
