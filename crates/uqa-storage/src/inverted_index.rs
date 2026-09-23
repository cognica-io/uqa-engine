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
mod batch;
mod bindings;
mod changes;
mod contract;
pub mod defaults;
mod footprint;
mod memory;
mod metadata;
mod read_cursor;
mod retained;
mod snapshot;

#[cfg(test)]
mod tests;

pub use analysis::{
    analyze_index_field, analyze_index_field_budgeted, analyze_index_field_cancellable,
    analyze_query_graph, analyze_query_graph_budgeted, analyze_query_terms,
    analyze_query_terms_budgeted, AnalyzedField, IndexedFieldMetadata,
};
pub use bindings::{AnalyzerBindings, AnalyzerDefault, RetainedAnalyzerBindings};
pub use changes::{visit_field_replacement, InvertedIndexChange, InvertedIndexChangeVisitor};
pub use contract::{AnalyzerPhase, InvertedIndex};
pub use metadata::IndexedFieldRevision;
pub use retained::RetainedInvertedIndexBuilder;

/// Linear term/position stores cannot install morphology without immutable graph revisions.
pub fn validate_linear_analyzer(analyzer: &Analyzer) -> StorageBackendResult<()> {
    if analyzer.uses_korean_stages() {
        return Err(StorageBackendError::Other("Korean analyzers require immutable analyzer revisions and lossless token-graph storage".into()));
    }
    if analyzer.uses_japanese_stages() {
        return Err(StorageBackendError::Other("Japanese analyzers require immutable analyzer revisions and lossless token-graph storage".into()));
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

#[derive(Debug)]
pub struct MemoryInvertedIndex {
    bindings: AnalyzerBindings,
    /// Snapshots share immutable corpus state; validated writes detach it once.
    state: Arc<MemoryIndexState>,
    /// Controlled readers keep the shared generation's allowance after the source changes or closes.
    state_memory: Option<Arc<uqa_core::memory::MemoryReservation>>,
    read_control: Option<crate::read_control::StorageReadControl>,
}

/// Cloning retains the existing independently owned corpus maps; snapshot APIs share them until a write.
impl Clone for MemoryInvertedIndex {
    fn clone(&self) -> Self {
        Self {
            bindings: self.bindings.clone(),
            state: Arc::new((*self.state).clone()),
            state_memory: None,
            read_control: None,
        }
    }
}

#[derive(Debug, Default)]
struct MemoryIndexState {
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
    retention: footprint::RetainedPayload,
}

type PostingKey = (FieldName, TokenTermKey);

#[derive(Debug, Clone)]
struct MemoryPosting {
    projection: PostingEntry,
    occurrences: Vec<TokenOccurrence>,
}

impl MemoryPosting {
    fn new(doc_id: DocId, occurrences: Vec<TokenOccurrence>, mut positions: Vec<u32>) -> Self {
        positions.sort_unstable();
        positions.dedup();
        Self {
            projection: PostingEntry::new(
                doc_id,
                Payload {
                    positions,
                    score: 0.0,
                    fields: BTreeMap::new(),
                },
            ),
            occurrences,
        }
    }
}

struct StagedMemoryDocument {
    fields: BTreeMap<FieldName, IndexedFieldMetadata>,
    terms: BTreeSet<PostingKey>,
    postings: Vec<(PostingKey, MemoryPosting)>,
}

struct MemoryReplacementPlan {
    old_terms: BTreeSet<PostingKey>,
    next_doc_count: u64,
    field_counters: BTreeMap<FieldName, MemoryFieldCounters>,
}

struct MemoryFieldCounters {
    total_key: FieldName,
    total: u64,
    docs: u64,
}

impl MemoryInvertedIndex {
    pub fn new(analyzer: Analyzer) -> Self {
        Self::with_bindings(AnalyzerBindings::new(analyzer))
    }

    fn with_bindings(bindings: AnalyzerBindings) -> Self {
        Self {
            bindings,
            state: Arc::default(),
            state_memory: None,
            read_control: None,
        }
    }

    fn shared_snapshot(&self) -> Self {
        Self {
            bindings: self.bindings.clone(),
            state: Arc::clone(&self.state),
            state_memory: self.state_memory.clone(),
            read_control: self.read_control.clone(),
        }
    }

    fn check_retained_read(&self) -> StorageBackendResult<()> {
        self.read_control
            .as_ref()
            .map_or(Ok(()), crate::read_control::StorageReadControl::check)
    }

    fn stage_document(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<StagedMemoryDocument> {
        self.stage_document_inner(doc_id, fields, None)
    }

    fn stage_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> StorageBackendResult<StagedMemoryDocument> {
        let mut metadata = BTreeMap::new();
        let mut terms = BTreeSet::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            let revision = self.bindings.index_revision(&field)?;
            let analyzed = match cancellation {
                Some(cancellation) => {
                    analyze_index_field_cancellable(&revision, &text, cancellation)?
                }
                None => analyze_index_field(&revision, &text)?,
            };
            metadata.insert(
                field.clone(),
                IndexedFieldMetadata::new(&revision, &analyzed),
            );
            for (term, occurrences) in analyzed.terms {
                if let Some(cancellation) = cancellation {
                    cancellation.check()?;
                }
                let positions = occurrences.iter().map(|item| item.position).collect();
                let key = (field.clone(), term);
                terms.insert(key.clone());
                postings.push((key, MemoryPosting::new(doc_id, occurrences, positions)));
            }
        }
        Ok(StagedMemoryDocument {
            fields: metadata,
            terms,
            postings,
        })
    }
}

impl MemoryIndexState {
    fn plan_replacement(
        &self,
        doc_id: DocId,
        new_fields: &BTreeMap<FieldName, IndexedFieldMetadata>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        self.plan_replacement_with_names(doc_id, new_fields, |field| Ok(field.clone()))
    }

    fn plan_replacement_with_names(
        &self,
        doc_id: DocId,
        new_fields: &BTreeMap<FieldName, IndexedFieldMetadata>,
        mut copy_name: impl FnMut(&FieldName) -> StorageBackendResult<FieldName>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        let has_terms = self.doc_terms.contains_key(&doc_id);
        if has_terms != self.doc_fields.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(format!(
                "inverted-index document {doc_id} has inconsistent reverse-index state"
            )));
        }
        let old_terms = self.doc_terms.get(&doc_id).cloned().unwrap_or_default();
        let empty_fields = BTreeMap::new();
        let old_fields = self.doc_fields.get(&doc_id).unwrap_or(&empty_fields);
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
        affected_fields.extend(old_fields.keys());
        affected_fields.extend(new_fields.keys());
        let mut field_counters = BTreeMap::new();
        for field in affected_fields {
            let old_length = old_fields.get(field).map_or(0, |metadata| metadata.length);
            let new_length = new_fields.get(field).map_or(0, |metadata| metadata.length);
            let total = self
                .total_length
                .get(field)
                .copied()
                .unwrap_or(0)
                .checked_sub(old_length)
                .ok_or_else(|| counter_error("total field length"))?
                .checked_add(new_length)
                .ok_or_else(|| counter_error("total field length"))?;
            let field_docs = self
                .field_doc_counts
                .get(field)
                .copied()
                .unwrap_or(0)
                .checked_sub(u64::from(old_fields.contains_key(field)))
                .ok_or_else(|| counter_error("field document count"))?
                .checked_add(u64::from(new_fields.contains_key(field)))
                .ok_or_else(|| counter_error("field document count"))?;
            field_counters.insert(
                copy_name(field)?,
                MemoryFieldCounters {
                    total_key: copy_name(field)?,
                    total,
                    docs: field_docs,
                },
            );
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
            self.remove_posting(doc_id, &key)?;
        }
        self.remove_document_metadata(doc_id);
        for (field, counters) in plan.field_counters {
            footprint::set_counter(
                &mut self.total_length,
                counters.total_key,
                (counters.docs != 0).then_some(counters.total),
                &mut self.retention,
            );
            footprint::set_counter(
                &mut self.field_doc_counts,
                field,
                (counters.docs != 0).then_some(counters.docs),
                &mut self.retention,
            );
        }
        for (key, entry) in staged.postings {
            self.insert_posting(doc_id, key, entry);
        }
        self.doc_count = plan.next_doc_count;
        if !staged.fields.is_empty() {
            self.insert_document_metadata(doc_id, staged.fields, staged.terms);
        }
        Ok(())
    }
}
