//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inverted index abstraction and an in-memory implementation.
//!
//! The index maps `(field, term)` keys to posting lists, tracks per-field
//! token lengths and corpus statistics, and indexes documents by running
//! an [`Analyzer`] over each field's text.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList, TokenOccurrence};

use crate::TokenTermKey;

use crate::backend::{StorageBackendError, StorageBackendResult};
use crate::block_max_index::BlockMaxScorer;
use crate::clustered_postings::{MaterializedPostingCursor, PostingCursor, PostingScore};

mod analysis;
mod bindings;
mod contract;
mod memory;

#[cfg(test)]
mod tests;

pub use analysis::{analyze_index_field, AnalyzedField, IndexedFieldMetadata};
pub use bindings::AnalyzerBindings;
pub use contract::{AnalyzerPhase, InvertedIndex};

/// Linear term/position stores cannot install Korean analysis without immutable graph revisions.
pub fn validate_linear_analyzer(analyzer: &Analyzer) -> StorageBackendResult<()> {
    if analyzer.uses_korean_stages() {
        return Err(StorageBackendError::Other("Korean analyzers require immutable analyzer revisions and lossless token-graph storage".into()));
    }
    Ok(())
}

/// A linear store must not accept a revision whose declared normalization policy it cannot preserve.
pub fn validate_linear_revision(
    revision: &uqa_analysis::CompiledAnalyzer,
) -> StorageBackendResult<()> {
    validate_linear_analyzer(&revision.descriptor().configuration()?)?;
    if revision.descriptor().length_policy() != uqa_analysis::TokenLengthPolicy::EmittedTokens {
        return Err(StorageBackendError::Other(
            "overlap-discounted analyzer revisions require occurrence storage".into(),
        ));
    }
    Ok(())
}

fn counter_error(context: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("inverted-index {context} overflow or corruption"))
}

fn usize_to_u64(value: usize, context: &str) -> StorageBackendResult<u64> {
    u64::try_from(value).map_err(|_| counter_error(context))
}

fn checked_sum_u64(
    values: impl IntoIterator<Item = u64>,
    context: &str,
) -> StorageBackendResult<u64> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| counter_error(context))
    })
}

pub(crate) fn validate_token_position_count(token_count: u64) -> StorageBackendResult<()> {
    // Positions are zero-based, so a stream containing u32::MAX + 1 tokens
    // still has a representable final position (u32::MAX).
    if token_count > u64::from(u32::MAX) + 1 {
        return Err(StorageBackendError::Other(
            "document token positions exceed the u32 index format".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct MemoryInvertedIndex {
    bindings: AnalyzerBindings,
    /// `(field, term) -> doc_id -> entry (positions inside the doc)`
    index: BTreeMap<PostingKey, BTreeMap<DocId, MemoryPosting>>,
    /// Reverse index for `remove_document` so we touch only relevant
    /// `(field, term)` posting maps instead of scanning the whole index.
    doc_terms: BTreeMap<DocId, BTreeSet<PostingKey>>,
    /// Exact revision, normalization length, and original source end state for each document field.
    doc_fields: BTreeMap<DocId, BTreeMap<FieldName, IndexedFieldMetadata>>,
    /// Sum of field lengths across all docs, per field.
    total_length: BTreeMap<FieldName, u64>,
    /// Number of documents with indexed content per field, maintained
    /// incrementally so per-query BM25 statistics never walk
    /// `doc_fields` (O(corpus) at query time otherwise).
    field_doc_counts: BTreeMap<FieldName, u64>,
    doc_count: u64,
}

type PostingKey = (FieldName, TokenTermKey);

#[derive(Debug, Clone)]
struct MemoryPosting {
    projection: PostingEntry,
    occurrences: Vec<TokenOccurrence>,
}

struct StagedMemoryDocument {
    fields: BTreeMap<FieldName, IndexedFieldMetadata>,
    terms: BTreeSet<PostingKey>,
    postings: Vec<(PostingKey, MemoryPosting)>,
}

struct MemoryReplacementPlan {
    old_terms: BTreeSet<PostingKey>,
    next_doc_count: u64,
    field_counters: BTreeMap<FieldName, (u64, u64)>,
}

impl MemoryInvertedIndex {
    pub fn new(analyzer: Analyzer) -> Self {
        Self::with_bindings(AnalyzerBindings::new(analyzer))
    }

    fn with_bindings(bindings: AnalyzerBindings) -> Self {
        Self {
            bindings,
            index: BTreeMap::new(),
            doc_terms: BTreeMap::new(),
            doc_fields: BTreeMap::new(),
            total_length: BTreeMap::new(),
            field_doc_counts: BTreeMap::new(),
            doc_count: 0,
        }
    }

    fn stage_document(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<StagedMemoryDocument> {
        let mut metadata = BTreeMap::new();
        let mut terms = BTreeSet::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            let revision = self.bindings.index_revision(&field)?;
            let analyzed = analyze_index_field(&revision, &text)?;
            metadata.insert(
                field.clone(),
                IndexedFieldMetadata::new(&revision, &analyzed),
            );
            for (term, occurrences) in analyzed.terms {
                let mut positions: Vec<_> = occurrences.iter().map(|item| item.position).collect();
                positions.sort_unstable();
                positions.dedup();
                let key = (field.clone(), term);
                terms.insert(key.clone());
                postings.push((
                    key,
                    MemoryPosting {
                        projection: PostingEntry::new(
                            doc_id,
                            Payload {
                                positions,
                                score: 0.0,
                                fields: BTreeMap::new(),
                            },
                        ),
                        occurrences,
                    },
                ));
            }
        }
        Ok(StagedMemoryDocument {
            fields: metadata,
            terms,
            postings,
        })
    }

    fn plan_replacement(
        &self,
        doc_id: DocId,
        new_fields: &BTreeMap<FieldName, IndexedFieldMetadata>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        let has_terms = self.doc_terms.contains_key(&doc_id);
        if has_terms != self.doc_fields.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(format!(
                "inverted-index document {doc_id} has inconsistent reverse-index state"
            )));
        }
        let old_terms = self.doc_terms.get(&doc_id).cloned().unwrap_or_default();
        let old_fields = self.doc_fields.get(&doc_id).cloned().unwrap_or_default();
        let next_doc_count = self
            .doc_count
            .checked_sub(u64::from(has_terms))
            .ok_or_else(|| counter_error("document count"))?
            .checked_add(u64::from(!new_fields.is_empty()))
            .ok_or_else(|| counter_error("document count"))?;
        for key in &old_terms {
            if !self
                .index
                .get(key)
                .is_some_and(|postings| postings.contains_key(&doc_id))
            {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} references a missing posting"
                )));
            }
        }

        let mut affected_fields = BTreeSet::new();
        affected_fields.extend(old_fields.keys().cloned());
        affected_fields.extend(new_fields.keys().cloned());
        let mut field_counters = BTreeMap::new();
        for field in affected_fields {
            let old_length = old_fields.get(&field).map_or(0, |metadata| metadata.length);
            let new_length = new_fields.get(&field).map_or(0, |metadata| metadata.length);
            let total = self
                .total_length
                .get(&field)
                .copied()
                .unwrap_or(0)
                .checked_sub(old_length)
                .ok_or_else(|| counter_error("total field length"))?
                .checked_add(new_length)
                .ok_or_else(|| counter_error("total field length"))?;
            let field_docs = self
                .field_doc_counts
                .get(&field)
                .copied()
                .unwrap_or(0)
                .checked_sub(u64::from(old_fields.contains_key(&field)))
                .ok_or_else(|| counter_error("field document count"))?
                .checked_add(u64::from(new_fields.contains_key(&field)))
                .ok_or_else(|| counter_error("field document count"))?;
            field_counters.insert(field, (total, field_docs));
        }
        Ok(MemoryReplacementPlan {
            old_terms,
            next_doc_count,
            field_counters,
        })
    }

    fn apply_replacement(
        &mut self,
        doc_id: DocId,
        staged: StagedMemoryDocument,
        plan: MemoryReplacementPlan,
    ) -> StorageBackendResult<()> {
        for key in plan.old_terms {
            let postings = self.index.get_mut(&key).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} lost a validated posting before replacement"
                ))
            })?;
            postings.remove(&doc_id);
            if postings.is_empty() {
                self.index.remove(&key);
            }
        }
        self.doc_fields.remove(&doc_id);
        self.doc_terms.remove(&doc_id);
        for (field, (total, field_docs)) in plan.field_counters {
            if field_docs == 0 {
                self.total_length.remove(&field);
                self.field_doc_counts.remove(&field);
            } else {
                self.total_length.insert(field.clone(), total);
                self.field_doc_counts.insert(field, field_docs);
            }
        }
        for (key, entry) in staged.postings {
            self.index.entry(key).or_default().insert(doc_id, entry);
        }
        self.doc_count = plan.next_doc_count;
        if !staged.fields.is_empty() {
            self.doc_fields.insert(doc_id, staged.fields);
            self.doc_terms.insert(doc_id, staged.terms);
        }
        Ok(())
    }
}
