//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic source replacement with coalesced graph clusters and revision statistics.

use super::data::FieldStats;
use super::{
    clustered_result, encode_index_counter, encode_index_u64, invalidate_posting_accelerators,
    load_cluster, params, write_cluster, BTreeMap, DocId, FieldName, OccurrencePosting,
    SQLiteError, SQLiteInvertedIndex, SQLiteResult, StagedField, TokenTermKey,
};
use uqa_storage::clustered_postings::{cluster_id, encode_term_keys};

type Documents = BTreeMap<DocId, BTreeMap<FieldName, StagedField>>;
type Changes = BTreeMap<(FieldName, TokenTermKey, u64), BTreeMap<DocId, Option<OccurrencePosting>>>;

fn invalid(message: &str) -> SQLiteError {
    SQLiteError::StorageBackend(message.into())
}

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

fn add_statistics(
    totals: &mut BTreeMap<FieldName, FieldStats>,
    fields: &BTreeMap<FieldName, StagedField>,
) -> SQLiteResult<()> {
    for (field, snapshot) in fields {
        let revision = snapshot.metadata.revision();
        let stats = totals.entry(field.clone()).or_insert(FieldStats {
            revision,
            doc_count: 0,
            total_length: 0,
        });
        if stats.revision != revision {
            return Err(invalid(
                "changing a populated field's index revision requires an atomic source rebuild",
            ));
        }
        stats.doc_count = stats
            .doc_count
            .checked_add(1)
            .ok_or_else(|| invalid("field document count overflow"))?;
        stats.total_length = stats
            .total_length
            .checked_add(snapshot.metadata.length)
            .ok_or_else(|| invalid("total field length overflow"))?;
    }
    Ok(())
}

impl SQLiteInvertedIndex {
    fn stage_documents(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> SQLiteResult<Documents> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            encode_index_u64("document", doc_id)?;
            staged.insert(doc_id, self.analyze_fields(fields)?);
        }
        Ok(staged)
    }

    fn write_document_on(
        &self,
        conn: &rusqlite::Connection,
        doc_id: DocId,
        fields: &BTreeMap<FieldName, StagedField>,
    ) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", doc_id)?;
        for table in ["_occurrence_documents", "_occurrence_lengths"] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE table_name = ?1 AND doc_id = ?2"),
                params![self.table, doc_id],
            )?;
        }
        for (field, snapshot) in fields {
            let terms = snapshot.postings.keys().cloned().collect::<Vec<_>>();
            let terms = clustered_result(encode_term_keys(&terms))?;
            let metadata = clustered_result(snapshot.metadata.to_bytes())?;
            conn.execute("INSERT INTO _occurrence_documents(table_name, doc_id, field, terms_blob, metadata_blob) VALUES (?1, ?2, ?3, ?4, ?5)", params![self.table, doc_id, field, terms, metadata.as_slice()])?;
            conn.execute("INSERT INTO _occurrence_lengths(table_name, doc_id, field, length) VALUES (?1, ?2, ?3, ?4)", params![self.table, doc_id, field, encode_index_counter("document length", snapshot.metadata.length)?])?;
        }
        Ok(())
    }

    fn write_statistics_on(
        &self,
        conn: &rusqlite::Connection,
        totals: &BTreeMap<FieldName, FieldStats>,
    ) -> SQLiteResult<()> {
        for (field, stats) in totals {
            if stats.doc_count == 0 {
                if stats.total_length != 0 {
                    return Err(invalid("empty indexed field retains document length"));
                }
                conn.execute(
                    "DELETE FROM _occurrence_fields WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                )?;
            } else {
                let revision = clustered_result(stats.revision.to_bytes())?;
                conn.execute("INSERT INTO _occurrence_fields(table_name, field, revision, doc_count, total_length) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(table_name, field) DO UPDATE SET revision = excluded.revision, doc_count = excluded.doc_count, total_length = excluded.total_length", params![self.table, field, revision.as_slice(), encode_index_counter("field document count", stats.doc_count)?, encode_index_counter("total field length", stats.total_length)?])?;
            }
        }
        Ok(())
    }

    pub(super) fn add_documents_inner(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> SQLiteResult<()> {
        let staged = self.stage_documents(documents)?;
        if staged.is_empty() {
            return self.require_graph_format();
        }
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            self.require_graph_format_on(&tx)?;
            let mut totals = BTreeMap::<FieldName, FieldStats>::new();
            let mut changes = Changes::new();
            for (doc_id, fields) in &staged {
                let old = self.old_document_on(&tx, encode_index_u64("document", *doc_id)?)?;
                for field in old.keys().chain(fields.keys()) {
                    if !totals.contains_key(field) {
                        if let Some(stats) = self.stored_field_stats_on(&tx, field)? {
                            totals.insert(field.clone(), stats);
                        }
                    }
                }
                for (field, snapshot) in old {
                    let stats = totals
                        .get_mut(&field)
                        .ok_or_else(|| invalid("indexed field revision is missing"))?;
                    stats.doc_count = stats
                        .doc_count
                        .checked_sub(1)
                        .ok_or_else(|| invalid("field document count underflow"))?;
                    stats.total_length = stats
                        .total_length
                        .checked_sub(snapshot.metadata.length)
                        .ok_or_else(|| invalid("total field length underflow"))?;
                    for term in snapshot.postings.into_keys() {
                        changes
                            .entry((field.clone(), term, cluster_id(*doc_id)))
                            .or_default()
                            .insert(*doc_id, None);
                    }
                }
            }
            if totals.is_empty() && staged.values().all(BTreeMap::is_empty) {
                tx.commit()?;
                return Ok(());
            }
            for (doc_id, fields) in &staged {
                add_statistics(&mut totals, fields)?;
                for (field, snapshot) in fields {
                    for (term, occurrences) in &snapshot.postings {
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
            for ((field, term, cluster), updates) in changes {
                let merged = merge_cluster_changes(
                    load_cluster(&tx, &self.table, &field, &term, cluster)?,
                    updates,
                );
                write_cluster(&tx, &self.table, &field, &term, cluster, &merged)?;
            }
            for (doc_id, fields) in &staged {
                self.write_document_on(&tx, *doc_id, fields)?;
            }
            self.write_statistics_on(&tx, &totals)?;
            for field in totals.keys() {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_posting_accelerators(&tx, &self.table)?;
            self.publish_graph_format(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn add_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<()> {
        self.add_documents_inner(vec![(doc_id, fields)])
    }

    pub(super) fn remove_document_inner(&self, doc_id: DocId) -> SQLiteResult<()> {
        self.add_documents_inner(vec![(doc_id, BTreeMap::new())])
    }

    pub(super) fn rebuild_documents_inner(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> SQLiteResult<()> {
        let staged = self.stage_documents(documents)?;
        let mut totals = BTreeMap::new();
        let mut clusters =
            BTreeMap::<(FieldName, TokenTermKey, u64), Vec<OccurrencePosting>>::new();
        for (doc_id, fields) in &staged {
            add_statistics(&mut totals, fields)?;
            for (field, snapshot) in fields {
                for (term, occurrences) in &snapshot.postings {
                    clusters
                        .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                        .or_default()
                        .push(OccurrencePosting {
                            doc_id: *doc_id,
                            doc_length: snapshot.metadata.length,
                            occurrences: occurrences.clone(),
                        });
                }
            }
        }
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            self.clear_index_on(&tx)?;
            for ((field, term, cluster), entries) in clusters {
                write_cluster(&tx, &self.table, &field, &term, cluster, &entries)?;
            }
            for (doc_id, fields) in &staged {
                self.write_document_on(&tx, *doc_id, fields)?;
            }
            self.write_statistics_on(&tx, &totals)?;
            for field in totals.keys() {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_posting_accelerators(&tx, &self.table)?;
            tx.commit()?;
            Ok(())
        })
    }
}
