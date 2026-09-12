//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Clustered inverted-index adapter over an ordered key/value store.

use super::codec::{
    blob_to_positions, decode_u64_value, doc_length_doc_prefix, doc_length_key,
    doc_length_key_prefix, field_stats_key, field_stats_key_prefix, key_with_tag, other_error,
    posting_cluster_positions_key, posting_cluster_positions_key_prefix,
    posting_cluster_score_field_prefix, posting_cluster_score_key,
    posting_cluster_score_key_prefix, posting_cluster_score_term_prefix,
    posting_document_doc_prefix, posting_document_key, posting_document_key_prefix, read_str,
    read_u64, reverse_posting_key, single_str_key, string_value, u64_value, usize_to_u64,
};
use super::{
    Analyzer, AnalyzerPhase, Arc, BTreeMap, BTreeSet, DocId, FieldName, IndexStats, InvertedIndex,
    KeyValueBatch, KeyValueStore, Payload, PostingEntry, PostingList, StorageBackendResult,
    TAG_METADATA, TAG_POSTING, TAG_REVERSE_POSTING,
};
use crate::clustered_postings::{
    cluster_id, decode_all_scores, decode_cluster, decode_terms, encode_cluster, encode_terms,
    score_count, ClusterPosting, ClusteredPostingCursor, EncodedScoreCluster,
    MaterializedPostingCursor,
};
use crate::PostingCursor;

mod migration;
mod mutation;
mod trait_impl;

use migration::{migrate_legacy_forward_postings, migrate_legacy_reverse_postings};
use mutation::{accumulate_field_changes, merge_cluster_changes};

const FORMAT_METADATA_KEY: &str = "inverted_index_format";
const CLUSTERED_FORMAT_NAME: &str = "clustered-v1";
const MIGRATION_PAGE_SIZE: usize = 1_024;

/// Inverted index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueInvertedIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    bindings: crate::inverted_index::AnalyzerBindings,
}

type KeyValueStagedPosting = (FieldName, String, Vec<u32>);
type KeyValueAnalyzedFields = (BTreeMap<FieldName, u64>, Vec<KeyValueStagedPosting>);
type ClusterKey = (FieldName, String, u64);
type PostingChange = Option<(u64, Vec<u32>)>;
type KeyValueStagedDocuments = BTreeMap<DocId, KeyValueAnalyzedFields>;
type KeyValueClusterChanges = BTreeMap<ClusterKey, BTreeMap<DocId, PostingChange>>;
type KeyValueFieldChanges = BTreeMap<FieldName, (u64, u64)>;
type KeyValueMergedClusters = Vec<(ClusterKey, Vec<ClusterPosting>)>;

impl KeyValueInvertedIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        analyzer: Analyzer,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            bindings: crate::inverted_index::AnalyzerBindings::new(analyzer),
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
            return Err(other_error(
                "cannot migrate KeyValue postings inside an active transaction",
            ));
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

    fn old_doc_lengths(&self, doc_id: DocId) -> StorageBackendResult<BTreeMap<FieldName, u64>> {
        let mut out = BTreeMap::new();
        for (key, value) in self
            .store
            .scan_prefix(&doc_length_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            out.insert(field, decode_u64_value(&value)?);
        }
        Ok(out)
    }

    fn old_terms(&self, doc_id: DocId) -> StorageBackendResult<BTreeMap<FieldName, Vec<String>>> {
        let mut out = BTreeMap::new();
        for (key, value) in self
            .store
            .scan_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            out.insert(field, decode_terms(&value)?);
        }
        Ok(out)
    }

    fn analyze_fields(
        &self,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<KeyValueAnalyzedFields> {
        let mut lengths = BTreeMap::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            crate::inverted_index::validate_linear_analyzer(
                self.bindings.index_configuration(&field),
            )?;
            let tokens = self.bindings.index_revision(&field)?.analyze(&text)?;
            let token_count = usize_to_u64(tokens.len(), "document token count")?;
            crate::inverted_index::validate_token_position_count(token_count)?;
            lengths.insert(field.clone(), token_count);
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (position, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(
                    u32::try_from(position)
                        .map_err(|_| other_error("token position exceeds u32 index format"))?,
                );
            }
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                postings.push((field.clone(), term, positions));
            }
        }
        Ok((lengths, postings))
    }

    fn set_total_length(
        batch: &mut dyn KeyValueBatch,
        table: &str,
        field: &str,
        value: u64,
    ) -> StorageBackendResult<()> {
        let key = field_stats_key(table, field)?;
        if value == 0 {
            batch.delete(&key)
        } else {
            batch.put(&key, &u64_value(value))
        }
    }

    fn load_cluster(
        &self,
        field: &str,
        term: &str,
        posting_cluster: u64,
    ) -> StorageBackendResult<Vec<ClusterPosting>> {
        let score = self.store.get(&posting_cluster_score_key(
            &self.table,
            field,
            term,
            posting_cluster,
        )?)?;
        let positions = self.store.get(&posting_cluster_positions_key(
            &self.table,
            field,
            term,
            posting_cluster,
        )?)?;
        match (score, positions) {
            (None, None) => Ok(Vec::new()),
            (Some(score), Some(positions)) => decode_cluster(posting_cluster, &score, &positions),
            _ => Err(other_error(
                "clustered posting score and positions values disagree",
            )),
        }
    }

    fn stage_cluster_changes(
        &self,
        doc_id: DocId,
        old_terms: &BTreeMap<FieldName, Vec<String>>,
        lengths: &BTreeMap<FieldName, u64>,
        postings: &[KeyValueStagedPosting],
    ) -> StorageBackendResult<Vec<(ClusterKey, Vec<ClusterPosting>)>> {
        let mut changes = BTreeMap::<(FieldName, String), PostingChange>::new();
        for (field, terms) in old_terms {
            for term in terms {
                changes.insert((field.clone(), term.clone()), None);
            }
        }
        for (field, term, positions) in postings {
            changes.insert(
                (field.clone(), term.clone()),
                Some((lengths[field], positions.clone())),
            );
        }

        let posting_cluster = cluster_id(doc_id);
        let mut output = Vec::with_capacity(changes.len());
        for ((field, term), replacement) in changes {
            let mut entries = self.load_cluster(&field, &term, posting_cluster)?;
            if let Ok(position) = entries.binary_search_by_key(&doc_id, |entry| entry.doc_id) {
                entries.remove(position);
            }
            if let Some((doc_length, positions)) = replacement {
                let position = entries.partition_point(|entry| entry.doc_id < doc_id);
                entries.insert(
                    position,
                    ClusterPosting {
                        doc_id,
                        term_freq: positions.len() as u64,
                        doc_length,
                        positions,
                    },
                );
            }
            output.push(((field, term, posting_cluster), entries));
        }
        Ok(output)
    }

    fn apply_cluster_changes(
        batch: &mut dyn KeyValueBatch,
        table: &str,
        changes: Vec<(ClusterKey, Vec<ClusterPosting>)>,
    ) -> StorageBackendResult<()> {
        for ((field, term, posting_cluster), entries) in changes {
            let score_key = posting_cluster_score_key(table, &field, &term, posting_cluster)?;
            let positions_key =
                posting_cluster_positions_key(table, &field, &term, posting_cluster)?;
            if entries.is_empty() {
                batch.delete(&score_key)?;
                batch.delete(&positions_key)?;
            } else {
                let (score, positions) = encode_cluster(&entries)?;
                batch.put(&score_key, &score)?;
                batch.put(&positions_key, &positions)?;
            }
        }
        Ok(())
    }

    fn collect_batch_changes(
        &self,
        staged_documents: &KeyValueStagedDocuments,
    ) -> StorageBackendResult<(KeyValueClusterChanges, KeyValueFieldChanges)> {
        let mut cluster_changes = KeyValueClusterChanges::new();
        let mut field_changes = KeyValueFieldChanges::new();
        for (doc_id, (new_lengths, new_postings)) in staged_documents {
            let old_lengths = self.old_doc_lengths(*doc_id)?;
            let old_terms = self.old_terms(*doc_id)?;
            let posting_cluster = cluster_id(*doc_id);
            for (field, terms) in old_terms {
                for term in terms {
                    cluster_changes
                        .entry((field.clone(), term, posting_cluster))
                        .or_default()
                        .insert(*doc_id, None);
                }
            }
            for (field, term, positions) in new_postings {
                cluster_changes
                    .entry((field.clone(), term.clone(), posting_cluster))
                    .or_default()
                    .insert(*doc_id, Some((new_lengths[field], positions.clone())));
            }
            accumulate_field_changes(&mut field_changes, &old_lengths, new_lengths)?;
        }
        Ok((cluster_changes, field_changes))
    }

    fn plan_batch_totals(
        &self,
        field_changes: KeyValueFieldChanges,
    ) -> StorageBackendResult<Vec<(FieldName, u64)>> {
        let mut totals = Vec::with_capacity(field_changes.len());
        for (field, (old_total, new_total)) in field_changes {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let total = base
                .checked_sub(old_total)
                .ok_or_else(|| other_error("stored field length is smaller than batch length"))?
                .checked_add(new_total)
                .ok_or_else(|| other_error("total field length overflow"))?;
            totals.push((field, total));
        }
        Ok(totals)
    }

    fn merge_batch_clusters(
        &self,
        cluster_changes: KeyValueClusterChanges,
    ) -> StorageBackendResult<KeyValueMergedClusters> {
        let mut merged = Vec::with_capacity(cluster_changes.len());
        for ((field, term, posting_cluster), changes) in cluster_changes {
            let entries = self.load_cluster(&field, &term, posting_cluster)?;
            merged.push((
                (field, term, posting_cluster),
                merge_cluster_changes(entries, changes),
            ));
        }
        Ok(merged)
    }

    fn write_batch_documents(
        &self,
        batch: &mut dyn KeyValueBatch,
        staged_documents: KeyValueStagedDocuments,
    ) -> StorageBackendResult<()> {
        for (doc_id, (lengths, postings)) in staged_documents {
            batch.delete_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?;
            batch.delete_prefix(&doc_length_doc_prefix(&self.table, doc_id)?)?;
            let mut terms_by_field = BTreeMap::<FieldName, Vec<String>>::new();
            for (field, term, _) in postings {
                terms_by_field.entry(field).or_default().push(term);
            }
            for (field, length) in lengths {
                batch.put(
                    &doc_length_key(&self.table, doc_id, &field)?,
                    &u64_value(length),
                )?;
                batch.put(
                    &posting_document_key(&self.table, doc_id, &field)?,
                    &encode_terms(terms_by_field.get(&field).map_or(&[], Vec::as_slice))?,
                )?;
            }
        }
        Ok(())
    }

    fn add_documents(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut staged_documents = BTreeMap::new();
        for (doc_id, fields) in documents {
            staged_documents.insert(doc_id, self.analyze_fields(fields)?);
        }
        if staged_documents.is_empty() {
            return Ok(());
        }

        let (cluster_changes, field_changes) = self.collect_batch_changes(&staged_documents)?;
        let totals = self.plan_batch_totals(field_changes)?;
        let merged_clusters = self.merge_batch_clusters(cluster_changes)?;
        let mut batch = self.store.batch();
        Self::apply_cluster_changes(batch.as_mut(), &self.table, merged_clusters)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        self.write_batch_documents(batch.as_mut(), staged_documents)?;
        batch.commit()
    }

    fn cursor_for_term(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        let mut clusters = Vec::new();
        for (key, bytes) in self.store.scan_prefix(&posting_cluster_score_term_prefix(
            &self.table,
            field,
            term,
        )?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let posting_cluster = read_u64(&key, &mut offset)?;
            if offset != key.len() {
                return Err(other_error("invalid clustered posting score key"));
            }
            clusters.push(EncodedScoreCluster {
                cluster_id: posting_cluster,
                bytes,
            });
        }
        if clusters.is_empty() {
            return Ok(Box::new(MaterializedPostingCursor::new(Vec::new())?));
        }
        Ok(Box::new(ClusteredPostingCursor::new(clusters)?))
    }
}
