//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved encoded reads and graph decoding within one retained `SQLite` snapshot.

use super::{
    decode_index_u64, encode_index_u64, SQLiteError, SQLiteInvertedIndex, SQLiteResult,
    TokenTermKey,
};
use crate::read_control::{blob, payload_length, read_snapshot, reserve_bindings};
use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::{
    memory::{Budgeted, MemoryError},
    DocId, IndexStats, TokenOccurrence,
};
use uqa_storage::{
    clustered_postings::{
        cluster_id, decode_occurrence_document_budgeted, score_count_with_control,
        EncodedScoreClusterRef, ScoreClusterVisitor,
    },
    read_control::StorageReadControl,
};

mod metadata;

#[cfg(test)]
mod tests;

impl SQLiteInvertedIndex {
    pub(super) fn visit_clusters_budgeted(
        &self,
        field: &str,
        term: &TokenTermKey,
        after: Option<u64>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut ScoreClusterVisitor<'_>,
    ) -> SQLiteResult<()> {
        control.check()?;
        self.conn.with(|connection| read_snapshot(connection, |connection| {
            self.require_graph_format_budgeted_on(connection, control)?;
            if limit == 0 || after.is_some_and(|after| after >= cluster_id(u64::MAX)) { return Ok(()); }
            let _bindings = reserve_bindings(control, &[self.table.as_bytes(), field.as_bytes(), term.as_bytes()])?;
            let mut last = after.map(|after| encode_index_u64("posting cluster", after)).transpose()?;
            for _ in 0..limit {
                control.check()?;
                let sql = if last.is_some() {
                    "SELECT cluster_id, posting_count, CASE WHEN typeof(score_blob) = 'blob' THEN length(score_blob) ELSE -1 END FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND cluster_id > ?4 ORDER BY cluster_id LIMIT 1"
                } else {
                    "SELECT cluster_id, posting_count, CASE WHEN typeof(score_blob) = 'blob' THEN length(score_blob) ELSE -1 END FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND ?4 IS NULL ORDER BY cluster_id LIMIT 1"
                };
                let row: Option<(i64, i64, i64)> = connection.query_row(
                    sql,
                    params![self.table, field, term.as_bytes(), last], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                ).optional()?;
                let Some((id, count, length)) = row else { break; };
                let length = payload_length(length)?;
                control.check()?;
                let _payload = control.memory().reserve(length)?;
                let mut statement = connection.prepare("SELECT score_blob FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND cluster_id = ?4")?;
                let mut rows = statement.query(params![self.table, field, term.as_bytes(), id])?;
                let row = rows.next()?.ok_or_else(changed_snapshot)?;
                let bytes = blob(row, 0)?;
                if bytes.len() != length { return Err(changed_snapshot()); }
                control.check()?;
                visit(EncodedScoreClusterRef { cluster_id: decode_index_u64("posting cluster", id)?, stored_count: Some(decode_index_u64("posting count", count)?), bytes })?;
                control.check()?;
                last = Some(id);
            }
            control.check()?;
            Ok(())
        }))
    }

    pub(super) fn occurrences_budgeted(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> SQLiteResult<Budgeted<Vec<TokenOccurrence>>> {
        control.check()?;
        self.conn.with(|connection| read_snapshot(connection, |connection| {
            self.require_graph_format_budgeted_on(connection, control)?;
            let cluster = cluster_id(doc_id);
            let stored_cluster = encode_index_u64("posting cluster", cluster)?;
            let posting = {
                let _bindings = reserve_bindings(control, &[self.table.as_bytes(), field.as_bytes(), term.as_bytes()])?;
                let sizes: Option<(i64, i64, i64)> = connection.query_row(
                    "SELECT posting_count, CASE WHEN typeof(score_blob) = 'blob' THEN length(score_blob) ELSE -1 END, CASE WHEN typeof(positions_blob) = 'blob' THEN length(positions_blob) ELSE -1 END FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND cluster_id = ?4",
                    params![self.table, field, term.as_bytes(), stored_cluster], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                ).optional()?;
                control.check()?;
                let Some((stored_count, score_size, position_size)) = sizes else { return Ok(Budgeted::new(Vec::new(), control.memory().empty_reservation())); };
                let score_size = payload_length(score_size)?;
                let position_size = payload_length(position_size)?;
                control.check()?;
                let _payload = control.memory().reserve(score_size.checked_add(position_size).ok_or(MemoryError::SizeOverflow)?)?;
                let mut statement = connection.prepare("SELECT score_blob, positions_blob FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND cluster_id = ?4")?;
                let mut rows = statement.query(params![self.table, field, term.as_bytes(), stored_cluster])?;
                let row = rows.next()?.ok_or_else(changed_snapshot)?;
                let score = blob(row, 0)?;
                let positions = blob(row, 1)?;
                if score.len() != score_size || positions.len() != position_size { return Err(changed_snapshot()); }
                let posting = decode_occurrence_document_budgeted(cluster, score, positions, doc_id, control.memory(), || control.check())?;
                if decode_index_u64("posting count", stored_count)? != score_count_with_control(score, || control.check())? {
                    return Err(SQLiteError::StorageBackend("stored posting count disagrees with the score payload".into()));
                }
                posting
            };
            let Some(posting) = posting else { return Ok(Budgeted::new(Vec::new(), control.memory().empty_reservation())); };
            let metadata = self.metadata_budgeted_on(connection, doc_id, field, control)?;
            metadata.validate_posting(&posting, || control.check())?;
            control.check()?;
            let (posting, memory) = posting.into_parts();
            Ok(Budgeted::new(posting.occurrences, memory))
        }))
    }

    pub(super) fn scalar_stats_budgeted(
        &self,
        field: &str,
        control: &StorageReadControl,
    ) -> SQLiteResult<IndexStats> {
        control.check()?;
        self.conn.with(|connection| {
            read_snapshot(connection, |connection| {
                self.require_graph_format_budgeted_on(connection, control)?;
                let mut stats = IndexStats::default();
                if let Some(field) = self.field_stats_budgeted_on(connection, field, control)? {
                    stats.total_docs = field.doc_count;
                    stats.avg_doc_length = field.total_length as f64 / field.doc_count as f64;
                }
                control.check()?;
                Ok(stats)
            })
        })
    }
}

fn changed_snapshot() -> SQLiteError {
    SQLiteError::StorageBackend("encoded cluster changed within its read snapshot".into())
}
