//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary term lookup, complete graph reads, and score-only cursors.

use super::{
    clustered_result, decode_index_u64, encode_index_u64, params, params_from_iter,
    posting_cursor_from_rows, BTreeMap, BTreeSet, OccurrencePosting, Payload, PostingCursor,
    PostingEntry, PostingList, SQLiteError, SQLiteInvertedIndex, SQLiteResult, SqlValue,
    StorageBackendResult, TokenTermKey,
};
use uqa_storage::clustered_postings::decode_occurrence_cluster;

pub(super) fn project_postings(entries: Vec<OccurrencePosting>) -> PostingList {
    PostingList::from_sorted_unchecked(
        entries
            .into_iter()
            .map(|entry| {
                PostingEntry::new(
                    entry.doc_id,
                    Payload {
                        positions: entry.positions(),
                        score: 0.0,
                        fields: BTreeMap::new(),
                    },
                )
            })
            .collect(),
    )
}

impl SQLiteInvertedIndex {
    pub(super) fn validate_posting_metadata_on(
        &self,
        conn: &rusqlite::Connection,
        field: &str,
        posting: &OccurrencePosting,
    ) -> SQLiteResult<()> {
        let metadata = self
            .read_field_metadata_on(conn, encode_index_u64("document", posting.doc_id)?, field)?
            .ok_or_else(|| {
                SQLiteError::StorageBackend("occurrence source metadata is missing".into())
            })?;
        Ok(metadata.validate_posting(posting, || Ok(()))?)
    }

    pub(super) fn occurrence_postings_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Vec<OccurrencePosting>>> {
        let unique = terms
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let mut by_term = BTreeMap::<TokenTermKey, Vec<OccurrencePosting>>::new();
            for chunk in unique.chunks(900) {
                let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(", ");
                let sql = format!("SELECT term, cluster_id, posting_count, score_blob, positions_blob FROM _occurrence_clusters WHERE table_name = ? AND field = ? AND term IN ({placeholders}) ORDER BY term, cluster_id");
                let mut values = vec![SqlValue::Text(self.table.clone()), SqlValue::Text(field.into())];
                values.extend(chunk.iter().map(|term| SqlValue::Blob(term.as_bytes().to_vec())));
                let mut statement = conn.prepare(&sql)?;
                let rows = statement.query_map(params_from_iter(values), |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, Vec<u8>>(4)?)))?;
                for row in rows {
                    let (term, cluster, count, score, graph) = row?;
                    let cluster = decode_index_u64("posting cluster", cluster)?;
                    let count = decode_index_u64("posting count", count)?;
                    let entries = clustered_result(decode_occurrence_cluster(cluster, &score, &graph))?;
                    if count != entries.len() as u64 {
                        return Err(SQLiteError::StorageBackend("corrupt clustered posting: stored posting count mismatch".into()));
                    }
                    for entry in &entries {
                        self.validate_posting_metadata_on(conn, field, entry)?;
                    }
                    by_term.entry(clustered_result(TokenTermKey::from_bytes(term))?).or_default().extend(entries);
                }
            }
            Ok(terms.iter().map(|term| by_term.get(term).cloned().unwrap_or_default()).collect())
        })?)
    }

    pub(super) fn cursor_for_term(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let mut statement = conn.prepare_cached("SELECT cluster_id, posting_count, score_blob FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 ORDER BY cluster_id")?;
            let rows = statement.query_map(params![self.table, field, term.as_bytes()], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, Vec<u8>>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            posting_cursor_from_rows(rows)
        })?)
    }

    pub(super) fn cursors_for_terms(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        let unique = terms
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let mut by_term = BTreeMap::<TokenTermKey, Vec<(i64, i64, Vec<u8>)>>::new();
            for chunk in unique.chunks(900) {
                let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(", ");
                let sql = format!("SELECT term, cluster_id, posting_count, score_blob FROM _occurrence_clusters WHERE table_name = ? AND field = ? AND term IN ({placeholders}) ORDER BY term, cluster_id");
                let mut values = vec![SqlValue::Text(self.table.clone()), SqlValue::Text(field.into())];
                values.extend(chunk.iter().map(|term| SqlValue::Blob(term.as_bytes().to_vec())));
                let mut statement = conn.prepare(&sql)?;
                let rows = statement.query_map(params_from_iter(values), |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?)))?;
                for row in rows {
                    let (term, cluster, count, score) = row?;
                    by_term.entry(clustered_result(TokenTermKey::from_bytes(term))?).or_default().push((cluster, count, score));
                }
            }
            let mut cursors = BTreeMap::new();
            for term in &unique {
                cursors.insert(term.clone(), posting_cursor_from_rows(by_term.remove(term).unwrap_or_default())?);
            }
            Ok(terms.iter().map(|term| cursors[term].clone()).collect())
        })?)
    }
}

impl SQLiteInvertedIndex {
    pub(super) fn term_frequencies_on(
        &self,
        conn: &rusqlite::Connection,
        field: Option<&str>,
    ) -> SQLiteResult<BTreeMap<(String, TokenTermKey), u64>> {
        self.require_graph_format_on(conn)?;
        let mut statement = conn.prepare("SELECT field, term, cluster_id, posting_count, score_blob FROM _occurrence_clusters WHERE table_name = ?1 AND (?2 IS NULL OR field = ?2) ORDER BY field, term, cluster_id")?;
        let rows = statement.query_map(params![self.table, field], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut frequencies = BTreeMap::new();
        for row in rows {
            let (field, term, cluster, count, bytes) = row?;
            let count = posting_cursor_from_rows(vec![(cluster, count, bytes)])?.doc_freq();
            let term = clustered_result(TokenTermKey::from_bytes(term))?;
            let frequency = frequencies.entry((field, term)).or_insert(0_u64);
            *frequency = frequency
                .checked_add(count)
                .ok_or_else(|| SQLiteError::StorageBackend("document frequency overflow".into()))?;
        }
        Ok(frequencies)
    }
}
