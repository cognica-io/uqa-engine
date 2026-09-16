//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Skip positions and scorer-versioned bounds use the canonical occurrence view and mutation boundary.

use super::{
    keys, other_error, BTreeMap, DocId, KeyValueBatch, KeyValueInvertedIndex, OccurrenceRead,
    StorageBackendResult, TokenTermKey,
};
use crate::block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
use crate::key_value::{
    codec::{decode_u64_value, u64_value},
    occurrence_format::BlockMaxValue,
};

impl KeyValueInvertedIndex {
    pub fn flush_skip_pointers(&self) -> StorageBackendResult<()> {
        self.mutate(|view, batch| view.rebuild_skips(batch))
    }

    pub fn skip_to(
        &self,
        field: &str,
        term: &str,
        target: DocId,
    ) -> StorageBackendResult<(DocId, usize)> {
        self.read(|view| view.skip_to(field, &TokenTermKey::from_text(term), target))
    }

    pub fn build_block_max_scores_key<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        term: &TokenTermKey,
        scorer: &S,
        fingerprint: &str,
    ) -> StorageBackendResult<()> {
        self.mutate(|view, batch| {
            view.build_bounds(batch, field, Some(term), scorer, fingerprint, false)
        })
    }

    pub fn build_all_block_max_scores<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        scorer: &S,
    ) -> StorageBackendResult<()> {
        self.mutate(|view, batch| view.build_bounds(batch, field, None, scorer, "", false))
    }

    pub(super) fn rebuild_block_max(
        &self,
        field: &str,
        scorer: &dyn BlockMaxScorer,
        fingerprint: &str,
    ) -> StorageBackendResult<bool> {
        if fingerprint.is_empty() {
            return Err(other_error(
                "persisted block-max scorer fingerprint must not be empty",
            ));
        }
        self.mutate(|view, batch| {
            view.build_bounds(batch, field, None, scorer, fingerprint, true)
        })?;
        Ok(true)
    }

    pub fn get_block_max_score(
        &self,
        field: &str,
        term: &str,
        block: usize,
    ) -> StorageBackendResult<f64> {
        self.read(|view| {
            view.require_graph_format()?;
            let block = u64::try_from(block).map_err(|_| other_error("block index exceeds u64"))?;
            let key = keys::cluster_key(
                view.table,
                keys::BLOCK_MAX,
                field,
                &TokenTermKey::from_text(term),
                block,
            )?;
            view.store
                .get(&key)?
                .map_or(Ok(0.0), |bytes| Ok(BlockMaxValue::decode(&bytes)?.score))
        })
    }

    pub fn get_all_block_max_scores_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<f64>> {
        self.read(|view| Ok(view.bounds(field, term, None)?.unwrap_or_default()))
    }

    pub fn get_versioned_block_max_scores_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
        fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        self.read(|view| {
            terms
                .iter()
                .map(|term| view.bounds(field, term, Some(fingerprint)))
                .collect()
        })
    }

    pub fn load_block_max_into(&self, target: &mut BlockMaxIndex) -> StorageBackendResult<()> {
        self.read(|view| {
            view.require_graph_format()?;
            let prefix = keys::kind_prefix(view.table, keys::BLOCK_MAX)?;
            let mut by_term = BTreeMap::<(String, TokenTermKey), Vec<f64>>::new();
            view.store.visit_prefix(&prefix, &mut |key, bytes| {
                view.store.control().check()?;
                let (field, term, ordinal) = keys::read_cluster(key, keys::BLOCK_MAX)?;
                let scores = by_term.entry((field, term)).or_default();
                if ordinal != scores.len() as u64 {
                    return Err(other_error("non-contiguous occurrence block-max bounds"));
                }
                scores.push(BlockMaxValue::decode(bytes)?.score);
                Ok(())
            })?;
            for ((field, term), scores) in by_term {
                target.set_block_maxes_key(view.table, &field, &term, scores)?;
            }
            Ok(())
        })
    }
}

impl OccurrenceRead<'_> {
    pub(super) fn invalidate_accelerators(
        &self,
        batch: &mut dyn KeyValueBatch,
    ) -> StorageBackendResult<()> {
        for kind in [keys::SKIP, keys::BLOCK_MAX] {
            batch.delete_prefix(&keys::kind_prefix(self.table, kind)?)?;
        }
        Ok(())
    }

    fn fence_accelerators(&self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        // Canonical writes also write this format record. A build or invalidation based on an older source cannot publish after the competing change, including caches absent from its original view.
        batch.put(
            &keys::kind_prefix(self.table, keys::FORMAT)?,
            keys::FORMAT_NAME,
        )
    }

    fn rebuild_skips(&self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        self.require_graph_format()?;
        batch.delete_prefix(&keys::kind_prefix(self.table, keys::SKIP)?)?;
        let fields = self.field_names()?;
        for field in &fields {
            for term in self.vocabulary_keys(field)? {
                let mut cursor = self.posting_cursor_key(field, &term)?;
                while let Some(entry) = cursor.current() {
                    self.store.control().check()?;
                    let offset = cursor.ordinal();
                    if offset % DEFAULT_BLOCK_SIZE as u64 == 0 {
                        batch.put(
                            &keys::cluster_key(self.table, keys::SKIP, field, &term, entry.doc_id)?,
                            &u64_value(offset),
                        )?;
                    }
                    cursor.advance()?;
                }
            }
        }
        if !fields.is_empty() {
            self.fence_accelerators(batch)?;
        }
        Ok(())
    }

    fn skip_to(
        &self,
        field: &str,
        term: &TokenTermKey,
        target: DocId,
    ) -> StorageBackendResult<(DocId, usize)> {
        self.require_graph_format()?;
        let prefix = keys::term_prefix(self.table, keys::SKIP, field, term)?;
        let mut found = (0, 0);
        self.store.visit_prefix(&prefix, &mut |key, bytes| {
            self.store.control().check()?;
            let (_, _, doc_id) = keys::read_cluster(key, keys::SKIP)?;
            let offset = usize::try_from(decode_u64_value(bytes)?)
                .map_err(|_| other_error("skip offset exceeds usize"))?;
            if doc_id <= target {
                found = (doc_id, offset);
            }
            Ok(())
        })?;
        Ok(found)
    }

    fn build_bounds<S: BlockMaxScorer + ?Sized>(
        &self,
        batch: &mut dyn KeyValueBatch,
        field: &str,
        term: Option<&TokenTermKey>,
        scorer: &S,
        fingerprint: &str,
        replace: bool,
    ) -> StorageBackendResult<()> {
        self.require_graph_format()?;
        if replace {
            batch.delete_prefix(&keys::field_prefix(self.table, keys::BLOCK_MAX, field)?)?;
        }
        if self.field_doc_count(field)? == 0 {
            return Ok(());
        }
        if let Some(term) = term {
            self.build_term_bounds(batch, field, term, scorer, fingerprint)?;
        } else {
            for term in self.vocabulary_keys(field)? {
                self.build_term_bounds(batch, field, &term, scorer, fingerprint)?;
            }
        }
        self.fence_accelerators(batch)
    }

    fn build_term_bounds<S: BlockMaxScorer + ?Sized>(
        &self,
        batch: &mut dyn KeyValueBatch,
        field: &str,
        term: &TokenTermKey,
        scorer: &S,
        fingerprint: &str,
    ) -> StorageBackendResult<()> {
        batch.delete_prefix(&keys::term_prefix(
            self.table,
            keys::BLOCK_MAX,
            field,
            term,
        )?)?;
        let mut cursor = self.posting_cursor_key(field, term)?;
        let mut count = 0usize;
        let mut block = 0u64;
        let mut maximum = 0.0_f64;
        while let Some(entry) = cursor.current() {
            self.store.control().check()?;
            let score = scorer.score(entry.term_freq, entry.doc_length, cursor.doc_freq());
            crate::block_max_index::validate_score(score)?;
            maximum = maximum.max(score);
            count += 1;
            if count == DEFAULT_BLOCK_SIZE {
                self.put_bound(batch, field, term, block, maximum, fingerprint)?;
                block = block
                    .checked_add(1)
                    .ok_or_else(|| other_error("block index overflow"))?;
                count = 0;
                maximum = 0.0;
            }
            cursor.advance()?;
        }
        if count != 0 {
            self.put_bound(batch, field, term, block, maximum, fingerprint)?;
        }
        Ok(())
    }

    fn put_bound(
        &self,
        batch: &mut dyn KeyValueBatch,
        field: &str,
        term: &TokenTermKey,
        block: u64,
        score: f64,
        fingerprint: &str,
    ) -> StorageBackendResult<()> {
        let value = BlockMaxValue { score, fingerprint }.encode(self.store.control())?;
        batch.put(
            &keys::cluster_key(self.table, keys::BLOCK_MAX, field, term, block)?,
            &value,
        )
    }

    fn bounds(
        &self,
        field: &str,
        term: &TokenTermKey,
        fingerprint: Option<&str>,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        self.require_graph_format()?;
        let prefix = keys::term_prefix(self.table, keys::BLOCK_MAX, field, term)?;
        let mut scores = Vec::new();
        let mut matches = true;
        self.store.visit_prefix(&prefix, &mut |key, bytes| {
            self.store.control().check()?;
            let (_, _, block) = keys::read_cluster(key, keys::BLOCK_MAX)?;
            if block != scores.len() as u64 {
                return Err(other_error("non-contiguous occurrence block-max bounds"));
            }
            let bound = BlockMaxValue::decode(bytes)?;
            matches &= fingerprint.is_none_or(|fingerprint| bound.fingerprint == fingerprint);
            scores.push(bound.score);
            Ok(())
        })?;
        Ok((matches && !scores.is_empty()).then_some(scores))
    }
}
