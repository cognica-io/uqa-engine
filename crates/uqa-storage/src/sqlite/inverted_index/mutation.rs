//! Transactional posting/document mutation.

use super::{
    corrupt_counter, decode_index_u64, encode_index_counter, encode_index_u64,
    invalidate_block_max_tables, load_document_lengths, load_field_total, params, BTreeMap,
    BTreeSet, DocId, FieldName, SQLiteInvertedIndex, SQLiteResult,
};

impl SQLiteInvertedIndex {
    pub(super) fn add_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", doc_id)?;
        let staged = self.analyze_fields(fields)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, doc_id)?;
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
                    params![self.table, field, doc_id],
                    |row| row.get(0),
                )?;
                let has_field_after = decode_index_u64("field document count", other_docs)? > 0
                    || staged.contains_key(&field);
                planned_totals.push((field, total, has_field_after));
            }

            // No data mutation occurs until every analyzer, conversion, and
            // counter transition has been validated.
            for field in staged.keys() {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
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
                let length = encode_index_counter("document length", staged_field.length)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _doc_lengths
                        (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![self.table, doc_id, field, length],
                )?;
                for (term, blob) in staged_field.postings {
                    tx.execute(
                        "INSERT OR REPLACE INTO _postings
                            (table_name, field, term, doc_id, positions)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![self.table, field, term, doc_id, blob],
                    )?;
                }
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
        for staged_fields in staged_documents.values() {
            for (field, staged_field) in staged_fields {
                let total = field_totals.entry(field.clone()).or_default();
                *total = total
                    .checked_add(staged_field.length)
                    .ok_or_else(|| corrupt_counter("total field length overflow"))?;
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
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.execute(
                "DELETE FROM _field_stats WHERE table_name = ?1",
                params![self.table],
            )?;

            {
                let mut insert_length = tx.prepare(
                    "INSERT OR REPLACE INTO _doc_lengths
                        (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                let mut insert_posting = tx.prepare(
                    "INSERT OR REPLACE INTO _postings
                        (table_name, field, term, doc_id, positions)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;

                for (doc_id, fields) in staged_documents {
                    for (field, staged_field) in fields {
                        let length = encode_index_counter("document length", staged_field.length)?;
                        insert_length.execute(params![self.table, doc_id, field, length])?;
                        for (term, blob) in staged_field.postings {
                            insert_posting
                                .execute(params![self.table, field, term, doc_id, blob])?;
                        }
                    }
                }
            }

            {
                let mut insert_stats = tx.prepare(
                    "INSERT INTO _field_stats (table_name, field, total_length)
                     VALUES (?1, ?2, ?3)",
                )?;
                for (field, total_length) in field_totals {
                    insert_stats.execute(params![
                        self.table,
                        field,
                        encode_index_counter("total field length", total_length)?
                    ])?;
                }
            }

            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn remove_document_inner(&self, doc_id: DocId) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", doc_id)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, doc_id)?;
            let mut planned_totals = Vec::with_capacity(old_lengths.len());
            for (field, length) in &old_lengths {
                let current = load_field_total(&tx, &self.table, field)?.unwrap_or(0);
                let total = current
                    .checked_sub(*length)
                    .ok_or_else(|| corrupt_counter("total field length underflow"))?;
                let other_docs: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 AND doc_id <> ?3",
                    params![self.table, field, doc_id],
                    |row| row.get(0),
                )?;
                planned_totals.push((
                    field.clone(),
                    encode_index_counter("total field length", total)?,
                    decode_index_u64("field document count", other_docs)? > 0,
                ));
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
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
