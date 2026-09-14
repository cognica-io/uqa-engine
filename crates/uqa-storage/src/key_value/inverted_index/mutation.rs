//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic document replacement with coalesced occurrence-cluster writes.

use super::super::codec::u64_value;
use super::{
    cluster_id, encode_occurrence_cluster, encode_term_keys, keys, other_error, BTreeMap,
    ClusterChanges, DocId, DocumentFields, FieldName, FieldStats, KeyValueBatch,
    KeyValueInvertedIndex, OccurrencePosting, StorageBackendResult, TokenTermKey,
};

fn merge_cluster_changes(
    entries: Vec<OccurrencePosting>,
    changes: BTreeMap<DocId, Option<OccurrencePosting>>,
) -> Vec<OccurrencePosting> {
    let mut merged = Vec::with_capacity(entries.len().saturating_add(changes.len()));
    let mut changes = changes.into_iter().peekable();
    for entry in entries {
        while changes.peek().is_some_and(|(id, _)| *id < entry.doc_id) {
            merged.extend(changes.next().expect("peeked change exists").1);
        }
        if changes.peek().is_some_and(|(id, _)| *id == entry.doc_id) {
            merged.extend(changes.next().expect("peeked change exists").1);
        } else {
            merged.push(entry);
        }
    }
    merged.extend(changes.filter_map(|(_, entry)| entry));
    merged
}

impl KeyValueInvertedIndex {
    pub(super) fn put_cluster(
        &self,
        batch: &mut dyn KeyValueBatch,
        field: &str,
        term: &TokenTermKey,
        cluster: u64,
        entries: &[OccurrencePosting],
    ) -> StorageBackendResult<()> {
        let score_key = keys::cluster_key(&self.table, keys::SCORE, field, term, cluster)?;
        let graph_key = keys::cluster_key(&self.table, keys::POSITIONS, field, term, cluster)?;
        if entries.is_empty() {
            batch.delete(&score_key)?;
            batch.delete(&graph_key)?;
        } else {
            let (score, graph) = encode_occurrence_cluster(entries)?;
            batch.put(&score_key, &score)?;
            batch.put(&graph_key, &graph)?;
        }
        Ok(())
    }

    pub(super) fn put_document(
        &self,
        batch: &mut dyn KeyValueBatch,
        doc_id: DocId,
        fields: &DocumentFields,
    ) -> StorageBackendResult<()> {
        for (field, snapshot) in fields {
            batch.put(
                &keys::metadata_key(&self.table, field, doc_id)?,
                &snapshot.metadata.to_bytes()?,
            )?;
            batch.put(
                &keys::document_key(&self.table, keys::LENGTH, doc_id, field)?,
                &u64_value(snapshot.metadata.length),
            )?;
            let terms = snapshot.terms.keys().cloned().collect::<Vec<_>>();
            batch.put(
                &keys::document_key(&self.table, keys::DOCUMENT, doc_id, field)?,
                &encode_term_keys(&terms)?,
            )?;
        }
        Ok(())
    }

    pub(super) fn add_field_statistics(
        totals: &mut BTreeMap<FieldName, FieldStats>,
        fields: &DocumentFields,
    ) -> StorageBackendResult<()> {
        for (field, snapshot) in fields {
            let revision = snapshot.metadata.revision();
            let stats = totals.entry(field.clone()).or_insert(FieldStats {
                revision,
                doc_count: 0,
                total_length: 0,
            });
            if stats.revision != revision {
                return Err(other_error(
                    "indexed field revisions disagree during replacement",
                ));
            }
            stats.doc_count = stats
                .doc_count
                .checked_add(1)
                .ok_or_else(|| other_error("field document count overflow"))?;
            stats.total_length = stats
                .total_length
                .checked_add(snapshot.metadata.length)
                .ok_or_else(|| other_error("total field length overflow"))?;
        }
        Ok(())
    }

    pub(super) fn put_field_statistics(
        &self,
        batch: &mut dyn KeyValueBatch,
        totals: BTreeMap<FieldName, FieldStats>,
    ) -> StorageBackendResult<()> {
        for (field, stats) in totals {
            let key = keys::field_prefix(&self.table, keys::FIELD, &field)?;
            if stats.doc_count == 0 {
                if stats.total_length != 0 {
                    return Err(other_error("empty indexed field retains document length"));
                }
                batch.delete(&key)?;
            } else {
                batch.put(&key, &stats.to_bytes()?)?;
            }
        }
        Ok(())
    }

    pub(super) fn add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.require_graph_format()?;
        let staged = self.stage_documents(documents, false)?;
        let mut previous = BTreeMap::new();
        let mut totals = BTreeMap::<FieldName, FieldStats>::new();
        let mut changes = ClusterChanges::new();
        for (doc_id, fields) in &staged {
            let old = self.old_document(*doc_id)?;
            for field in old.keys().chain(fields.keys()) {
                if !totals.contains_key(field) {
                    if let Some(stats) = self.stored_field_stats(field)? {
                        totals.insert(field.clone(), stats);
                    }
                }
            }
            for (field, snapshot) in &old {
                let stats = totals
                    .get_mut(field)
                    .ok_or_else(|| other_error("indexed field revision is missing"))?;
                stats.doc_count = stats
                    .doc_count
                    .checked_sub(1)
                    .ok_or_else(|| other_error("field document count underflow"))?;
                stats.total_length = stats
                    .total_length
                    .checked_sub(snapshot.metadata.length)
                    .ok_or_else(|| other_error("total field length underflow"))?;
                for term in snapshot.terms.keys() {
                    changes
                        .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                        .or_default()
                        .insert(*doc_id, None);
                }
            }
            previous.insert(*doc_id, old);
        }
        for (doc_id, fields) in &staged {
            Self::add_field_statistics(&mut totals, fields)?;
            for (field, snapshot) in fields {
                for (term, occurrences) in &snapshot.terms {
                    changes
                        .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                        .or_default()
                        .insert(
                            *doc_id,
                            Some(OccurrencePosting {
                                doc_id: *doc_id,
                                doc_length: snapshot.metadata.length,
                                occurrences: occurrences.clone(),
                            }),
                        );
                }
            }
        }
        if totals.is_empty() {
            return Ok(());
        }
        let mut batch = self.store.batch();
        for ((field, term, cluster), updates) in changes {
            let merged = merge_cluster_changes(self.load_cluster(&field, &term, cluster)?, updates);
            self.put_cluster(batch.as_mut(), &field, &term, cluster, &merged)?;
        }
        for (doc_id, fields) in previous {
            batch.delete_prefix(&keys::document_prefix(&self.table, keys::DOCUMENT, doc_id)?)?;
            batch.delete_prefix(&keys::document_prefix(&self.table, keys::LENGTH, doc_id)?)?;
            for field in fields.keys() {
                batch.delete(&keys::metadata_key(&self.table, field, doc_id)?)?;
            }
        }
        for (doc_id, fields) in &staged {
            self.put_document(batch.as_mut(), *doc_id, fields)?;
        }
        self.put_field_statistics(batch.as_mut(), totals)?;
        batch.put(
            &keys::kind_prefix(&self.table, keys::FORMAT)?,
            keys::FORMAT_NAME,
        )?;
        batch.commit()
    }
}
