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

const FORMAT_METADATA_KEY: &str = "inverted_index_format";
const CLUSTERED_FORMAT_NAME: &str = "clustered-v1";
const MIGRATION_PAGE_SIZE: usize = 1_024;

/// Inverted index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueInvertedIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    analyzer: Analyzer,
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

type KeyValueStagedPosting = (FieldName, String, Vec<u32>);
type KeyValueAnalyzedFields = (BTreeMap<FieldName, u64>, Vec<KeyValueStagedPosting>);
type ClusterKey = (FieldName, String, u64);
type PostingChange = Option<(u64, Vec<u32>)>;
type KeyValueStagedDocuments = BTreeMap<DocId, KeyValueAnalyzedFields>;
type KeyValueClusterChanges = BTreeMap<ClusterKey, BTreeMap<DocId, PostingChange>>;
type KeyValueFieldChanges = BTreeMap<FieldName, (u64, u64)>;
type KeyValueMergedClusters = Vec<(ClusterKey, Vec<ClusterPosting>)>;

fn merge_cluster_changes(
    entries: Vec<ClusterPosting>,
    changes: BTreeMap<DocId, PostingChange>,
) -> Vec<ClusterPosting> {
    fn push_replacement(
        entries: &mut Vec<ClusterPosting>,
        doc_id: DocId,
        replacement: PostingChange,
    ) {
        if let Some((doc_length, positions)) = replacement {
            entries.push(ClusterPosting {
                doc_id,
                term_freq: positions.len() as u64,
                doc_length,
                positions,
            });
        }
    }

    let mut merged = Vec::with_capacity(entries.len().saturating_add(changes.len()));
    let mut changes = changes.into_iter().peekable();
    for entry in entries {
        while changes
            .peek()
            .is_some_and(|(doc_id, _)| *doc_id < entry.doc_id)
        {
            let (doc_id, replacement) = changes.next().expect("peeked posting change exists");
            push_replacement(&mut merged, doc_id, replacement);
        }
        if changes
            .peek()
            .is_some_and(|(doc_id, _)| *doc_id == entry.doc_id)
        {
            let (doc_id, replacement) = changes.next().expect("peeked posting change exists");
            push_replacement(&mut merged, doc_id, replacement);
        } else {
            merged.push(entry);
        }
    }
    for (doc_id, replacement) in changes {
        push_replacement(&mut merged, doc_id, replacement);
    }
    merged
}

fn accumulate_field_changes(
    field_changes: &mut KeyValueFieldChanges,
    old_lengths: &BTreeMap<FieldName, u64>,
    new_lengths: &BTreeMap<FieldName, u64>,
) -> StorageBackendResult<()> {
    let mut affected_fields = BTreeSet::new();
    affected_fields.extend(old_lengths.keys().cloned());
    affected_fields.extend(new_lengths.keys().cloned());
    for field in affected_fields {
        let (old_total, new_total) = field_changes.entry(field.clone()).or_default();
        if let Some(length) = old_lengths.get(&field) {
            *old_total = old_total
                .checked_add(*length)
                .ok_or_else(|| other_error("old field length overflow"))?;
        }
        if let Some(length) = new_lengths.get(&field) {
            *new_total = new_total
                .checked_add(*length)
                .ok_or_else(|| other_error("new field length overflow"))?;
        }
    }
    Ok(())
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
            analyzer,
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
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
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text)?;
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

fn migrate_legacy_forward_postings(store: &dyn KeyValueStore) -> StorageBackendResult<u64> {
    let posting_prefix = key_with_tag(TAG_POSTING);
    let mut after = None::<Vec<u8>>;
    let mut group = None::<(String, String, String, u64, Vec<ClusterPosting>)>;
    let mut posting_count = 0_u64;
    loop {
        let page =
            store.scan_prefix_after(&posting_prefix, after.as_deref(), MIGRATION_PAGE_SIZE)?;
        if page.is_empty() {
            break;
        }
        for (key, value) in page {
            after = Some(key.clone());
            let (table, field, term, doc_id) = decode_legacy_posting_key(&key)?;
            let positions = blob_to_positions(&value)?;
            let doc_length = store
                .get(&doc_length_key(&table, doc_id, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .ok_or_else(|| {
                    other_error(format!(
                        "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing document length"
                    ))
                })?;
            if !store.contains_key(&reverse_posting_key(&table, doc_id, &field, &term)?)? {
                return Err(other_error(format!(
                    "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing reverse posting"
                )));
            }
            let posting_cluster = cluster_id(doc_id);
            let same_group = group.as_ref().is_some_and(
                |(group_table, group_field, group_term, group_cluster, _)| {
                    group_table == &table
                        && group_field == &field
                        && group_term == &term
                        && *group_cluster == posting_cluster
                },
            );
            if !same_group {
                if let Some(cluster) = group.take() {
                    put_migrated_cluster(store, cluster)?;
                }
                group = Some((table, field, term, posting_cluster, Vec::new()));
            }
            group
                .as_mut()
                .expect("posting migration group exists")
                .4
                .push(ClusterPosting {
                    doc_id,
                    term_freq: positions.len() as u64,
                    doc_length,
                    positions,
                });
            posting_count = posting_count
                .checked_add(1)
                .ok_or_else(|| other_error("legacy posting count overflow"))?;
            store.delete(&key)?;
        }
    }
    if let Some(cluster) = group {
        put_migrated_cluster(store, cluster)?;
    }
    Ok(posting_count)
}

fn migrate_legacy_reverse_postings(store: &dyn KeyValueStore) -> StorageBackendResult<u64> {
    let reverse_prefix = key_with_tag(TAG_REVERSE_POSTING);
    let mut after = None::<Vec<u8>>;
    let mut group = None::<(String, DocId, FieldName, Vec<String>)>;
    let mut reverse_count = 0_u64;
    loop {
        let page =
            store.scan_prefix_after(&reverse_prefix, after.as_deref(), MIGRATION_PAGE_SIZE)?;
        if page.is_empty() {
            break;
        }
        for (key, _) in page {
            after = Some(key.clone());
            let (table, doc_id, field, term) = decode_legacy_reverse_key(&key)?;
            let same_group =
                group
                    .as_ref()
                    .is_some_and(|(group_table, group_doc_id, group_field, _)| {
                        group_table == &table && *group_doc_id == doc_id && group_field == &field
                    });
            if !same_group {
                if let Some(document) = group.take() {
                    put_migrated_document(store, document)?;
                }
                group = Some((table, doc_id, field, Vec::new()));
            }
            group
                .as_mut()
                .expect("reverse posting migration group exists")
                .3
                .push(term);
            reverse_count = reverse_count
                .checked_add(1)
                .ok_or_else(|| other_error("legacy reverse posting count overflow"))?;
            store.delete(&key)?;
        }
    }
    if let Some(document) = group {
        put_migrated_document(store, document)?;
    }
    Ok(reverse_count)
}

impl InvertedIndex for KeyValueInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let (new_lengths, new_postings) = self.analyze_fields(fields)?;
        let cluster_changes =
            self.stage_cluster_changes(doc_id, &old_terms, &new_lengths, &new_postings)?;

        let mut fields_to_update = BTreeSet::new();
        fields_to_update.extend(old_lengths.keys().cloned());
        fields_to_update.extend(new_lengths.keys().cloned());
        let mut totals = Vec::with_capacity(fields_to_update.len());
        for field in fields_to_update {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let old = old_lengths.get(&field).copied().unwrap_or(0);
            let new = new_lengths.get(&field).copied().unwrap_or(0);
            let total = base
                .checked_sub(old)
                .ok_or_else(|| other_error("stored field length is smaller than document length"))?
                .checked_add(new)
                .ok_or_else(|| other_error("total field length overflow"))?;
            totals.push((field, total));
        }

        let mut terms_by_field = BTreeMap::<FieldName, Vec<String>>::new();
        for (field, term, _) in &new_postings {
            terms_by_field
                .entry(field.clone())
                .or_default()
                .push(term.clone());
        }
        for field in new_lengths.keys() {
            terms_by_field.entry(field.clone()).or_default();
        }

        let mut batch = self.store.batch();
        Self::apply_cluster_changes(batch.as_mut(), &self.table, cluster_changes)?;
        batch.delete_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?;
        for field in old_lengths.keys() {
            batch.delete(&doc_length_key(&self.table, doc_id, field)?)?;
        }
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for (field, length) in &new_lengths {
            batch.put(
                &doc_length_key(&self.table, doc_id, field)?,
                &u64_value(*length),
            )?;
        }
        for (field, terms) in terms_by_field {
            batch.put(
                &posting_document_key(&self.table, doc_id, &field)?,
                &encode_terms(&terms)?,
            )?;
        }
        batch.commit()
    }

    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.add_documents(documents)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let cluster_changes =
            self.stage_cluster_changes(doc_id, &old_terms, &BTreeMap::new(), &[])?;
        let mut totals = Vec::with_capacity(old_lengths.len());
        for (field, length) in &old_lengths {
            let base = self
                .store
                .get(&field_stats_key(&self.table, field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            totals.push((
                field.clone(),
                base.checked_sub(*length).ok_or_else(|| {
                    other_error("stored field length is smaller than removed document length")
                })?,
            ));
        }

        let mut batch = self.store.batch();
        Self::apply_cluster_changes(batch.as_mut(), &self.table, cluster_changes)?;
        batch.delete_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
            batch.delete(&doc_length_key(&self.table, doc_id, &field)?)?;
        }
        batch.commit()
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                staged.insert(doc_id, self.analyze_fields(fields)?);
            }
        }
        let mut totals = BTreeMap::<FieldName, u64>::new();
        let mut clusters = BTreeMap::<ClusterKey, Vec<ClusterPosting>>::new();
        for (doc_id, (lengths, postings)) in &staged {
            for (field, length) in lengths {
                let total = totals.entry(field.clone()).or_default();
                *total = total
                    .checked_add(*length)
                    .ok_or_else(|| other_error("total field length overflow"))?;
            }
            for (field, term, positions) in postings {
                clusters
                    .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                    .or_default()
                    .push(ClusterPosting {
                        doc_id: *doc_id,
                        term_freq: positions.len() as u64,
                        doc_length: lengths[field],
                        positions: positions.clone(),
                    });
            }
        }

        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_cluster_score_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_cluster_positions_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_document_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for ((field, term, posting_cluster), entries) in clusters {
            let (score, positions) = encode_cluster(&entries)?;
            batch.put(
                &posting_cluster_score_key(&self.table, &field, &term, posting_cluster)?,
                &score,
            )?;
            batch.put(
                &posting_cluster_positions_key(&self.table, &field, &term, posting_cluster)?,
                &positions,
            )?;
        }
        for (doc_id, (lengths, postings)) in staged {
            for (field, length) in lengths {
                batch.put(
                    &doc_length_key(&self.table, doc_id, &field)?,
                    &u64_value(length),
                )?;
                let terms = postings
                    .iter()
                    .filter(|(posting_field, _, _)| posting_field == &field)
                    .map(|(_, term, _)| term.clone())
                    .collect::<Vec<_>>();
                batch.put(
                    &posting_document_key(&self.table, doc_id, &field)?,
                    &encode_terms(&terms)?,
                )?;
            }
        }
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_cluster_score_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_cluster_positions_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_document_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.commit()
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        let mut entries = Vec::new();
        for (key, score) in self.store.scan_prefix(&posting_cluster_score_term_prefix(
            &self.table,
            field,
            term,
        )?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let posting_cluster = read_u64(&key, &mut offset)?;
            let positions = self
                .store
                .get(&posting_cluster_positions_key(
                    &self.table,
                    field,
                    term,
                    posting_cluster,
                )?)?
                .ok_or_else(|| other_error("clustered posting positions value is missing"))?;
            entries.extend(
                decode_cluster(posting_cluster, &score, &positions)?
                    .into_iter()
                    .map(|entry| {
                        PostingEntry::new(
                            entry.doc_id,
                            Payload {
                                positions: entry.positions,
                                score: 0.0,
                                fields: BTreeMap::new(),
                            },
                        )
                    }),
            );
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.cursor_for_term(field, term)
    }

    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        let mut cursor = self.cursor_for_term(field, term)?;
        while let Some(entry) = cursor.current() {
            visit(entry.doc_id, entry.term_freq);
            cursor.advance()?;
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.store
            .scan_prefix(&posting_cluster_score_term_prefix(
                &self.table,
                field,
                term,
            )?)?
            .into_iter()
            .try_fold(0_u64, |total, (_, score)| {
                total
                    .checked_add(score_count(&score)?)
                    .ok_or_else(|| other_error("document frequency overflow"))
            })
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&doc_length_key(&self.table, doc_id, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        let mut output = doc_ids
            .iter()
            .map(|doc_id| Ok((self.get_doc_length(*doc_id, field)?, vec![0; terms.len()])))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let mut positions = BTreeMap::<DocId, Vec<usize>>::new();
        for (position, doc_id) in doc_ids.iter().copied().enumerate() {
            positions.entry(doc_id).or_default().push(position);
        }
        for (term_index, term) in terms.iter().enumerate() {
            let mut cursor = self.cursor_for_term(field, term)?;
            while let Some(entry) = cursor.current() {
                if let Some(output_positions) = positions.get(&entry.doc_id) {
                    for position in output_positions {
                        output[*position].0 = entry.doc_length;
                        output[*position].1[term_index] = entry.term_freq;
                    }
                }
                cursor.advance()?;
            }
        }
        Ok(output)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        let posting_cluster = cluster_id(doc_id);
        self.store
            .get(&posting_cluster_score_key(
                &self.table,
                field,
                term,
                posting_cluster,
            )?)?
            .map_or(Ok(0), |score| {
                let entries = decode_all_scores(posting_cluster, &score)?;
                Ok(entries
                    .binary_search_by_key(&doc_id, |entry| entry.doc_id)
                    .ok()
                    .map_or(0, |position| entries[position].term_freq))
            })
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        let mut doc_ids = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            doc_ids.insert(read_u64(&key, &mut offset)?);
        }
        usize_to_u64(doc_ids.len(), "document count")
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&field_stats_key(&self.table, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        let mut terms = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&posting_cluster_score_field_prefix(&self.table, field)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            terms.insert(read_str(&key, &mut offset)?);
        }
        Ok(terms.into_iter().collect())
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let doc_count = self.doc_count()?;
        let mut stats = IndexStats::default();
        stats.total_docs = doc_count;
        if doc_count > 0 {
            let mut total = 0_u64;
            for (_, value) in self
                .store
                .scan_prefix(&field_stats_key_prefix(&self.table)?)?
            {
                total = total
                    .checked_add(decode_u64_value(&value)?)
                    .ok_or_else(|| other_error("index total field length overflow"))?;
            }
            stats.avg_doc_length = total as f64 / doc_count as f64;
        }
        let mut counts = BTreeMap::<(String, String), u64>::new();
        for (key, value) in self
            .store
            .scan_prefix(&posting_cluster_score_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            let count = counts.entry((field, term)).or_default();
            *count = count
                .checked_add(score_count(&value)?)
                .ok_or_else(|| other_error("index document frequency overflow"))?;
        }
        for ((field, term), document_frequency) in counts {
            stats.set_doc_freq(field, term, document_frequency);
        }
        Ok(stats)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let prefix = match field {
            Some(field) => posting_cluster_score_field_prefix(&self.table, field)?,
            None => posting_cluster_score_key_prefix(&self.table)?,
        };
        self.store
            .scan_prefix(&prefix)?
            .into_iter()
            .try_fold(0_u64, |total, (_, value)| {
                total
                    .checked_add(score_count(&value)?)
                    .ok_or_else(|| other_error("posting count overflow"))
            })
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut count = 0_u64;
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            if field.is_none_or(|target| target == indexed_field) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("document-length row count overflow"))?;
            }
        }
        Ok(count)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let prefix = match field {
            Some(field) => posting_cluster_score_field_prefix(&self.table, field)?,
            None => posting_cluster_score_key_prefix(&self.table)?,
        };
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&prefix)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let current_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            terms.insert((current_field, term));
        }
        usize_to_u64(terms.len(), "term count")
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        let mut fields = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(&field_stats_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            fields.push(read_str(&key, &mut offset)?);
        }
        Ok(fields)
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        match phase {
            AnalyzerPhase::Index => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Search => {
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Both => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer.clone());
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
        }
        Ok(())
    }

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        self.index_field_analyzers.remove(field);
        self.search_field_analyzers.remove(field);
        Ok(())
    }

    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.index_field_analyzers
            .get(field)
            .cloned()
            .unwrap_or_else(|| self.analyzer.clone())
    }

    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        if let Some(analyzer) = self.search_field_analyzers.get(field) {
            return analyzer.clone();
        }
        if let Some(analyzer) = self.index_field_analyzers.get(field) {
            return analyzer.clone();
        }
        self.analyzer.clone()
    }
}

fn decode_legacy_posting_key(
    key: &[u8],
) -> StorageBackendResult<(String, FieldName, String, DocId)> {
    let mut offset = 1;
    let table = read_str(key, &mut offset)?;
    let field = read_str(key, &mut offset)?;
    let term = read_str(key, &mut offset)?;
    let doc_id = read_u64(key, &mut offset)?;
    if offset != key.len() {
        return Err(other_error("invalid legacy posting key"));
    }
    Ok((table, field, term, doc_id))
}

fn decode_legacy_reverse_key(
    key: &[u8],
) -> StorageBackendResult<(String, DocId, FieldName, String)> {
    let mut offset = 1;
    let table = read_str(key, &mut offset)?;
    let doc_id = read_u64(key, &mut offset)?;
    let field = read_str(key, &mut offset)?;
    let term = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(other_error("invalid legacy reverse posting key"));
    }
    Ok((table, doc_id, field, term))
}

fn put_migrated_cluster(
    store: &dyn KeyValueStore,
    cluster: (String, FieldName, String, u64, Vec<ClusterPosting>),
) -> StorageBackendResult<()> {
    let (table, field, term, posting_cluster, entries) = cluster;
    let (score_blob, positions_blob) = encode_cluster(&entries)?;
    store.put(
        &posting_cluster_score_key(&table, &field, &term, posting_cluster)?,
        &score_blob,
    )?;
    store.put(
        &posting_cluster_positions_key(&table, &field, &term, posting_cluster)?,
        &positions_blob,
    )
}

fn put_migrated_document(
    store: &dyn KeyValueStore,
    document: (String, DocId, FieldName, Vec<String>),
) -> StorageBackendResult<()> {
    let (table, doc_id, field, mut terms) = document;
    // Length-prefixed key segments sort by encoded length before text bytes,
    // while the shared terms codec requires ordinary lexical ordering.
    terms.sort_unstable();
    store.put(
        &posting_document_key(&table, doc_id, &field)?,
        &encode_terms(&terms)?,
    )
}
