//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact field revisions, original-source metadata, and immutable analysis staging.

use super::{
    clustered_result, decode_index_u64, encode_index_counter, load_document_lengths, params,
    BTreeMap, FieldName, IndexedFieldMetadata, IndexedFieldRevision, OptionalExtension,
    SQLiteError, SQLiteInvertedIndex, SQLiteResult, StagedField, TokenTermKey,
};
use uqa_storage::clustered_postings::decode_term_keys;
use uqa_storage::inverted_index::{analyze_index_field, AnalyzerBindings};

#[derive(Clone, Copy)]
pub(super) struct FieldStats {
    pub revision: IndexedFieldRevision,
    pub doc_count: u64,
    pub total_length: u64,
}

impl SQLiteInvertedIndex {
    pub(super) fn stored_field_stats_on(
        &self,
        conn: &rusqlite::Connection,
        field: &str,
    ) -> SQLiteResult<Option<FieldStats>> {
        let row: Option<(Vec<u8>, i64, i64)> = conn.query_row(
            "SELECT revision, doc_count, total_length FROM _occurrence_fields WHERE table_name = ?1 AND field = ?2",
            params![self.table, field], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        row.map(|(revision, count, total)| {
            let doc_count = decode_index_u64("field document count", count)?;
            if doc_count == 0 {
                return Err(SQLiteError::StorageBackend(
                    "empty field statistics must be removed".into(),
                ));
            }
            Ok(FieldStats {
                revision: clustered_result(IndexedFieldRevision::from_bytes(&revision))?,
                doc_count,
                total_length: decode_index_u64("total field length", total)?,
            })
        })
        .transpose()
    }

    pub(super) fn validate_index_revision_change(
        &self,
        field: &str,
        candidate: &AnalyzerBindings,
    ) -> SQLiteResult<()> {
        self.conn.with(|conn| {
            if !self.needs_source_rebuild_on(conn)? {
                if let Some(stored) = self.stored_field_stats_on(conn, field)? {
                    let revision = candidate.index_revision(field)?;
                    if stored.revision != IndexedFieldRevision::new(&revision) {
                        return Err(SQLiteError::StorageBackend("changing a populated field's index revision requires an atomic source rebuild".into()));
                    }
                }
            }
            Ok(())
        })
    }

    pub(super) fn analyze_fields(
        &self,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<BTreeMap<FieldName, StagedField>> {
        let mut staged = BTreeMap::new();
        for (field, text) in fields {
            let revision = self.bindings.index_revision(&field)?;
            let analyzed = analyze_index_field(&revision, &text)?;
            let metadata = IndexedFieldMetadata::new(&revision, &analyzed);
            encode_index_counter("document length", metadata.length)?;
            staged.insert(
                field,
                StagedField {
                    metadata,
                    postings: analyzed.terms,
                },
            );
        }
        Ok(staged)
    }

    pub(super) fn read_field_metadata_on(
        &self,
        conn: &rusqlite::Connection,
        doc_id: i64,
        field: &str,
    ) -> SQLiteResult<Option<IndexedFieldMetadata>> {
        let bytes: Option<Vec<u8>> = conn.query_row(
            "SELECT metadata_blob FROM _occurrence_documents WHERE table_name = ?1 AND doc_id = ?2 AND field = ?3",
            params![self.table, doc_id, field], |row| row.get(0),
        ).optional()?;
        bytes
            .map(|bytes| {
                let metadata = clustered_result(IndexedFieldMetadata::from_bytes(&bytes))?;
                let stored = self.stored_field_stats_on(conn, field)?.ok_or_else(|| {
                    SQLiteError::StorageBackend("indexed field revision is missing".into())
                })?;
                if stored.revision != metadata.revision() {
                    return Err(SQLiteError::StorageBackend(
                        "indexed field revisions disagree".into(),
                    ));
                }
                Ok(metadata)
            })
            .transpose()
    }

    pub(super) fn old_document_on(
        &self,
        conn: &rusqlite::Connection,
        doc_id: i64,
    ) -> SQLiteResult<BTreeMap<FieldName, StagedField>> {
        let lengths = load_document_lengths(conn, &self.table, doc_id)?;
        let mut statement = conn.prepare("SELECT field, terms_blob FROM _occurrence_documents WHERE table_name = ?1 AND doc_id = ?2 ORDER BY field")?;
        let rows = statement.query_map(params![self.table, doc_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut fields = BTreeMap::new();
        for row in rows {
            let (field, terms) = row?;
            let metadata = self
                .read_field_metadata_on(conn, doc_id, &field)?
                .ok_or_else(|| {
                    SQLiteError::StorageBackend("indexed field end metadata is missing".into())
                })?;
            if lengths.get(&field).copied() != Some(metadata.length) {
                return Err(SQLiteError::StorageBackend(
                    "indexed field length disagrees with its source metadata".into(),
                ));
            }
            let postings = clustered_result(decode_term_keys(&terms))?
                .into_iter()
                .map(|term: TokenTermKey| (term, Vec::new()))
                .collect();
            fields.insert(field, StagedField { metadata, postings });
        }
        if fields.len() != lengths.len() {
            return Err(SQLiteError::StorageBackend(
                "indexed field reverse terms or metadata are missing".into(),
            ));
        }
        Ok(fields)
    }
}
