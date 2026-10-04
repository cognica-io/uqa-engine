//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic source replacement with coalesced graph clusters and revision statistics.

use super::clustered::write_encoded_cluster;
use super::data::FieldStats;
use super::{
    clustered_result, encode_index_counter, encode_index_u64, invalidate_posting_accelerators,
    load_cluster, params, write_cluster, BTreeMap, DocId, FieldName, OccurrencePosting,
    SQLiteError, SQLiteInvertedIndex, SQLiteResult, StagedField, TokenTermKey,
};
use uqa_storage::clustered_postings::{cluster_id, encode_term_keys};
use uqa_storage::inverted_index::{
    visit_field_replacement, InvertedIndexChange, InvertedIndexChangeVisitor, SourceRebuild,
    StagedFieldRecord, TextIndexSource,
};
use uqa_storage::read_control::StorageReadControl;

type DocumentFields = BTreeMap<FieldName, StagedField>;
type Documents = BTreeMap<DocId, DocumentFields>;
type Changes = BTreeMap<(FieldName, TokenTermKey, u64), BTreeMap<DocId, Option<OccurrencePosting>>>;

fn visit_replacement(
    visit: &mut InvertedIndexChangeVisitor<'_>,
    doc_id: DocId,
    old: &DocumentFields,
    fields: &DocumentFields,
) -> SQLiteResult<()> {
    for field in old
        .keys()
        .chain(fields.keys().filter(|field| !old.contains_key(*field)))
    {
        let before = old.get(field);
        let after = fields.get(field);
        visit_field_replacement(
            visit,
            doc_id,
            field,
            before.map(|snapshot| snapshot.metadata.length),
            after.map(|snapshot| snapshot.metadata.length),
        )
        .map_err(SQLiteError::from)?;
        for snapshot in before.into_iter().chain(after) {
            for term in snapshot.postings.keys() {
                visit(InvertedIndexChange::Posting {
                    doc_id,
                    field,
                    term,
                })
                .map_err(SQLiteError::from)?;
            }
        }
    }
    Ok(())
}

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
        self.stage_documents_inner(documents, None)
    }

    fn stage_documents_inner(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> SQLiteResult<Documents> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            encode_index_u64("document", doc_id)?;
            staged.insert(doc_id, self.analyze_fields(fields, cancellation)?);
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
        self.add_documents_observed(documents, None)
    }

    pub(super) fn add_documents_observed(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        mut visit: Option<&mut InvertedIndexChangeVisitor<'_>>,
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
                if let Some(visit) = visit.as_mut() {
                    visit_replacement(*visit, *doc_id, &old, fields)?;
                }
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
            for (doc_id, fields) in staged {
                add_statistics(&mut totals, &fields)?;
                self.write_document_on(&tx, doc_id, &fields)?;
                // The document metadata is now staged in this savepoint. Transfer its evaluated occurrences into cluster changes instead of retaining a second complete copy until publication.
                for (field, snapshot) in fields {
                    for (term, occurrences) in snapshot.postings {
                        changes
                            .entry((field.clone(), term, cluster_id(doc_id)))
                            .or_default()
                            .insert(
                                doc_id,
                                Some(OccurrencePosting {
                                    doc_id,
                                    doc_length: snapshot.metadata.length,
                                    occurrences,
                                }),
                            );
                    }
                }
            }
            let encoding = StorageReadControl::with_limit(usize::MAX);
            for ((field, term, cluster), updates) in changes {
                let merged = merge_cluster_changes(
                    load_cluster(&tx, &self.table, &field, &term, cluster)?,
                    updates,
                );
                write_cluster(&tx, &self.table, &field, &term, cluster, &merged, &encoding)?;
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
        source: &mut dyn TextIndexSource,
    ) -> SQLiteResult<()> {
        self.rebuild_documents_with_cancellation(source, None)
    }

    /// Replace the index with the documents `source` reads. They are analyzed and staged before the replacement begins, in a record set that spills beyond the default session allowance, so neither the source nor its postings are held in memory while the rows are written.
    pub(super) fn rebuild_documents_with_cancellation(
        &self,
        source: &mut dyn TextIndexSource,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> SQLiteResult<()> {
        let check = || cancellation.map_or(Ok(()), uqa_core::CancellationToken::check);
        check()?;
        let control = StorageReadControl::new(
            &uqa_core::memory::MemoryBudget::new(
                uqa_storage::mvcc::VersionedSessionOptions::default().retained_bytes,
            ),
            &cancellation.cloned().unwrap_or_default(),
        );
        let mut staged = SourceRebuild::new(&control);
        while let Some((doc_id, fields)) = source.next_document()? {
            check()?;
            encode_index_u64("document", doc_id)?;
            let analyzed = self.analyze_fields(fields, cancellation)?;
            staged.stage(
                doc_id,
                analyzed.iter().map(|(field, snapshot)| {
                    (field.as_str(), &snapshot.metadata, &snapshot.postings)
                }),
            )?;
        }
        let totals = staged
            .totals()
            .iter()
            .map(|(field, totals)| {
                (
                    field.clone(),
                    FieldStats {
                        revision: totals.revision,
                        doc_count: totals.doc_count,
                        total_length: totals.total_length,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            check()?;
            self.clear_index_on(&tx)?;
            staged
                .visit_clusters(&mut |cluster| {
                    check()?;
                    write_encoded_cluster(&tx, &self.table, &cluster)
                        .map_err(uqa_storage::StorageBackendError::from)
                })
                .map_err(SQLiteError::from)?;
            staged
                .visit_documents(&mut |record| {
                    check()?;
                    self.write_staged_field_on(&tx, &record)
                        .map_err(uqa_storage::StorageBackendError::from)
                })
                .map_err(SQLiteError::from)?;
            self.write_statistics_on(&tx, &totals)?;
            for field in totals.keys() {
                check()?;
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_posting_accelerators(&tx, &self.table)?;
            check()?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Write one staged field of a document into a replacement, whose rows the replacement cleared.
    fn write_staged_field_on(
        &self,
        conn: &rusqlite::Connection,
        record: &StagedFieldRecord<'_>,
    ) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", record.doc_id)?;
        let metadata = clustered_result(record.metadata.to_bytes())?;
        conn.execute("INSERT INTO _occurrence_documents(table_name, doc_id, field, terms_blob, metadata_blob) VALUES (?1, ?2, ?3, ?4, ?5)", params![self.table, doc_id, record.field, record.terms, metadata.as_slice()])?;
        conn.execute("INSERT INTO _occurrence_lengths(table_name, doc_id, field, length) VALUES (?1, ?2, ?3, ?4)", params![self.table, doc_id, record.field, encode_index_counter("document length", record.metadata.length)?])?;
        Ok(())
    }
}
