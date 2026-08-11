//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional clustered-posting and document mutation.

use super::{
    clustered_result, corrupt_counter, decode_index_u64, encode_index_counter, encode_index_u64,
    invalidate_block_max_tables, load_cluster, load_document_lengths, load_document_terms,
    load_field_total, params, write_cluster, BTreeMap, BTreeSet, ClusterPosting, DocId, FieldName,
    SQLiteInvertedIndex, SQLiteResult, StagedField,
};
use crate::clustered_postings::{cluster_id, encode_terms};

type PostingChange = Option<(u64, Vec<u32>)>;

fn clear_table_postings(conn: &rusqlite::Connection, table: &str) -> SQLiteResult<()> {
    for storage_table in [
        "_posting_clusters",
        "_posting_documents",
        "_doc_lengths",
        "_field_stats",
    ] {
        conn.execute(
            &format!("DELETE FROM {storage_table} WHERE table_name = ?1"),
            params![table],
        )?;
    }
    Ok(())
}

fn apply_document_postings(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    stored_doc_id: i64,
    old_terms: &BTreeMap<FieldName, Vec<String>>,
    staged: &BTreeMap<FieldName, StagedField>,
) -> SQLiteResult<()> {
    let mut changes = BTreeMap::<(FieldName, String), PostingChange>::new();
    for (field, terms) in old_terms {
        for term in terms {
            changes.insert((field.clone(), term.clone()), None);
        }
    }
    for (field, staged_field) in staged {
        for (term, positions) in &staged_field.postings {
            changes.insert(
                (field.clone(), term.clone()),
                Some((staged_field.length, positions.clone())),
            );
        }
    }

    let posting_cluster = cluster_id(doc_id);
    for ((field, term), replacement) in changes {
        let mut entries = load_cluster(conn, table, &field, &term, posting_cluster)?;
        match entries.binary_search_by_key(&doc_id, |entry| entry.doc_id) {
            Ok(position) => {
                entries.remove(position);
            }
            Err(position) => {
                if let Some((doc_length, positions)) = replacement {
                    entries.insert(
                        position,
                        ClusterPosting {
                            doc_id,
                            term_freq: positions.len() as u64,
                            doc_length,
                            positions,
                        },
                    );
                    write_cluster(conn, table, &field, &term, posting_cluster, &entries)?;
                    continue;
                }
            }
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
        write_cluster(conn, table, &field, &term, posting_cluster, &entries)?;
    }

    conn.execute(
        "DELETE FROM _posting_documents WHERE table_name = ?1 AND doc_id = ?2",
        params![table, stored_doc_id],
    )?;
    for (field, staged_field) in staged {
        let terms = staged_field
            .postings
            .iter()
            .map(|(term, _)| term.clone())
            .collect::<Vec<_>>();
        let terms_blob = clustered_result(encode_terms(&terms))?;
        conn.execute(
            "INSERT INTO _posting_documents (table_name, doc_id, field, terms_blob)
             VALUES (?1, ?2, ?3, ?4)",
            params![table, stored_doc_id, field, terms_blob],
        )?;
    }
    Ok(())
}

impl SQLiteInvertedIndex {
    pub(super) fn add_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<()> {
        let stored_doc_id = encode_index_u64("document", doc_id)?;
        let staged = self.analyze_fields(fields)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, stored_doc_id)?;
            let old_terms = load_document_terms(&tx, &self.table, stored_doc_id)?;
            let mut affected_fields = BTreeSet::new();
            affected_fields.extend(old_lengths.keys().cloned());
            affected_fields.extend(staged.keys().cloned());
            let mut planned_totals = Vec::with_capacity(affected_fields.len());
            for field in affected_fields {
                let current = load_field_total(&tx, &self.table, &field)?.unwrap_or(0);
                let old = old_lengths.get(&field).copied().unwrap_or(0);
                let new = staged.get(&field).map_or(0, |value| value.length);
                let total = current
                    .checked_sub(old)
                    .ok_or_else(|| corrupt_counter("total field length underflow"))?
                    .checked_add(new)
                    .ok_or_else(|| corrupt_counter("total field length overflow"))?;
                let total = encode_index_counter("total field length", total)?;
                let other_docs: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 AND doc_id <> ?3",
                    params![self.table, field, stored_doc_id],
                    |row| row.get(0),
                )?;
                let has_field_after = decode_index_u64("field document count", other_docs)? > 0
                    || staged.contains_key(&field);
                planned_totals.push((field, total, has_field_after));
            }

            for field in staged.keys() {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            apply_document_postings(&tx, &self.table, doc_id, stored_doc_id, &old_terms, &staged)?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, stored_doc_id],
            )?;
            for (field, total, has_field_after) in planned_totals {
                if has_field_after {
                    tx.execute(
                        "INSERT INTO _field_stats (table_name, field, total_length)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(table_name, field) DO UPDATE
                            SET total_length = excluded.total_length",
                        params![self.table, field, total],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM _field_stats WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field],
                    )?;
                }
            }
            for (field, staged_field) in staged {
                tx.execute(
                    "INSERT INTO _doc_lengths (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        self.table,
                        stored_doc_id,
                        field,
                        encode_index_counter("document length", staged_field.length)?
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn rebuild_documents_inner(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> SQLiteResult<()> {
        let mut staged_documents = BTreeMap::new();
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                staged_documents.insert(
                    encode_index_u64("document", doc_id)?,
                    self.analyze_fields(fields)?,
                );
            }
        }
        let fields = staged_documents
            .values()
            .flat_map(|fields| fields.keys().cloned())
            .collect::<BTreeSet<_>>();
        let mut field_totals = BTreeMap::<FieldName, u64>::new();
        let mut clusters = BTreeMap::<(FieldName, String, u64), Vec<ClusterPosting>>::new();
        for (stored_doc_id, staged_fields) in &staged_documents {
            let doc_id = decode_index_u64("document id", *stored_doc_id)?;
            for (field, staged_field) in staged_fields {
                let total = field_totals.entry(field.clone()).or_default();
                *total = total
                    .checked_add(staged_field.length)
                    .ok_or_else(|| corrupt_counter("total field length overflow"))?;
                for (term, positions) in &staged_field.postings {
                    clusters
                        .entry((field.clone(), term.clone(), cluster_id(doc_id)))
                        .or_default()
                        .push(ClusterPosting {
                            doc_id,
                            term_freq: positions.len() as u64,
                            doc_length: staged_field.length,
                            positions: positions.clone(),
                        });
                }
            }
        }

        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            for field in &fields {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            clear_table_postings(&tx, &self.table)?;

            for ((field, term, posting_cluster), entries) in clusters {
                write_cluster(&tx, &self.table, &field, &term, posting_cluster, &entries)?;
            }
            for (stored_doc_id, staged_fields) in staged_documents {
                for (field, staged_field) in staged_fields {
                    tx.execute(
                        "INSERT INTO _doc_lengths (table_name, doc_id, field, length)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            self.table,
                            stored_doc_id,
                            field,
                            encode_index_counter("document length", staged_field.length)?
                        ],
                    )?;
                    let terms = staged_field
                        .postings
                        .into_iter()
                        .map(|(term, _)| term)
                        .collect::<Vec<_>>();
                    tx.execute(
                        "INSERT INTO _posting_documents
                            (table_name, doc_id, field, terms_blob)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            self.table,
                            stored_doc_id,
                            field,
                            clustered_result(encode_terms(&terms))?
                        ],
                    )?;
                }
            }
            for (field, total_length) in field_totals {
                tx.execute(
                    "INSERT INTO _field_stats (table_name, field, total_length)
                     VALUES (?1, ?2, ?3)",
                    params![
                        self.table,
                        field,
                        encode_index_counter("total field length", total_length)?
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn remove_document_inner(&self, doc_id: DocId) -> SQLiteResult<()> {
        let stored_doc_id = encode_index_u64("document", doc_id)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, stored_doc_id)?;
            let old_terms = load_document_terms(&tx, &self.table, stored_doc_id)?;
            let mut planned_totals = Vec::with_capacity(old_lengths.len());
            for (field, length) in &old_lengths {
                let current = load_field_total(&tx, &self.table, field)?.unwrap_or(0);
                let total = current
                    .checked_sub(*length)
                    .ok_or_else(|| corrupt_counter("total field length underflow"))?;
                let other_docs: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 AND doc_id <> ?3",
                    params![self.table, field, stored_doc_id],
                    |row| row.get(0),
                )?;
                planned_totals.push((
                    field.clone(),
                    encode_index_counter("total field length", total)?,
                    decode_index_u64("field document count", other_docs)? > 0,
                ));
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            apply_document_postings(
                &tx,
                &self.table,
                doc_id,
                stored_doc_id,
                &old_terms,
                &BTreeMap::new(),
            )?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, stored_doc_id],
            )?;
            for (field, total, has_field_after) in planned_totals {
                if has_field_after {
                    tx.execute(
                        "UPDATE _field_stats SET total_length = ?3
                         WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field, total],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM _field_stats WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }
}
