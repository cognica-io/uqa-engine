//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Clustered occurrence indexes over an ordered key/value store.

use super::codec::{
    blob_to_positions, decode_u64_value, doc_length_key, key_with_tag, other_error,
    posting_cluster_positions_key, posting_cluster_score_key, posting_document_key, read_str,
    read_u64, reverse_posting_key, single_str_key, string_value,
};
use super::occurrence_keys as keys;
use super::{
    Analyzer, AnalyzerPhase, Arc, BTreeMap, BTreeSet, DocId, FieldName, IndexStats, InvertedIndex,
    KeyValueBatch, KeyValueStore, Payload, PostingEntry, PostingList, StorageBackendResult,
    TAG_METADATA, TAG_POSTING, TAG_REVERSE_POSTING,
};
use crate::clustered_postings::{
    cluster_id, decode_all_scores, decode_occurrence_cluster, decode_term_keys, encode_cluster,
    encode_occurrence_cluster, encode_term_keys, encode_terms, score_count, ClusterPosting,
    ClusteredPostingCursor, EncodedScoreCluster, OccurrencePosting,
};
use crate::inverted_index::{
    analyze_index_field, AnalyzerBindings, IndexedFieldMetadata, IndexedFieldRevision,
};
use crate::{PostingCursor, TokenTermKey};

mod controlled;
mod data;
mod format;
mod migration;
mod mutation;
mod queries;
mod rebuild;
mod trait_impl;

use migration::{migrate_legacy_forward_postings, migrate_legacy_reverse_postings};

const FORMAT_METADATA_KEY: &str = "inverted_index_format";
const CLUSTERED_FORMAT_NAME: &str = "clustered-v1";
const MIGRATION_PAGE_SIZE: usize = 1_024;

type ClusterKey = (FieldName, TokenTermKey, u64);
type DocumentFields = BTreeMap<FieldName, FieldSnapshot>;
type StagedDocuments = BTreeMap<DocId, DocumentFields>;
type ClusterChanges = BTreeMap<ClusterKey, BTreeMap<DocId, Option<OccurrencePosting>>>;

#[derive(Clone)]
struct FieldSnapshot {
    metadata: IndexedFieldMetadata,
    terms: BTreeMap<TokenTermKey, Vec<uqa_core::TokenOccurrence>>,
}

#[derive(Debug, Clone, Copy)]
struct FieldStats {
    revision: IndexedFieldRevision,
    doc_count: u64,
    total_length: u64,
}

/// Inverted index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueInvertedIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    bindings: AnalyzerBindings,
}

impl KeyValueInvertedIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        analyzer: Analyzer,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            bindings: AnalyzerBindings::new(analyzer),
        }
    }

    pub(crate) fn migrate_legacy_storage(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
        let marker = single_str_key(TAG_METADATA, FORMAT_METADATA_KEY)?;
        if let Some(format) = store.get(&marker)? {
            if format == CLUSTERED_FORMAT_NAME.as_bytes() {
                return Ok(());
            }
            return Err(other_error(format!(
                "unsupported KeyValue inverted-index format `{}`",
                String::from_utf8_lossy(&format)
            )));
        }
        if store.in_transaction() {
            return Self::migrate_legacy_storage_in_transaction(store, &marker);
        }

        store.begin_transaction()?;
        let migration = Self::migrate_legacy_storage_in_transaction(store, &marker);
        match migration {
            Ok(()) => store.commit_transaction(),
            Err(error) => match store.rollback_transaction() {
                Ok(()) => Err(error),
                Err(rollback) => Err(other_error(format!(
                    "{error}; KeyValue posting migration rollback also failed: {rollback}"
                ))),
            },
        }
    }

    fn migrate_legacy_storage_in_transaction(
        store: &dyn KeyValueStore,
        marker: &[u8],
    ) -> StorageBackendResult<()> {
        let posting_count = migrate_legacy_forward_postings(store)?;
        let reverse_count = migrate_legacy_reverse_postings(store)?;
        if posting_count != reverse_count {
            return Err(other_error(format!(
                "cannot migrate inconsistent KeyValue postings: {posting_count} forward rows and {reverse_count} reverse rows"
            )));
        }
        store.put(marker, &string_value(CLUSTERED_FORMAT_NAME))
    }
}
