//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` row helpers for the backend-neutral clustered posting codec.

use super::{
    decode_index_u64, encode_index_counter, encode_index_u64, params, ClusteredPostingCursor,
    EncodedScoreCluster, MaterializedPostingCursor, OccurrencePosting, OptionalExtension,
    PostingCursor, SQLiteError, SQLiteResult, TokenTermKey,
};
use uqa_storage::clustered_postings::{
    decode_occurrence_cluster, encode_occurrence_cluster, score_count,
};
use uqa_storage::StorageBackendResult;

pub(super) fn clustered_result<T>(result: StorageBackendResult<T>) -> SQLiteResult<T> {
    result.map_err(|error| SQLiteError::StorageBackend(error.to_string()))
}

pub(super) fn load_cluster(
    conn: &rusqlite::Connection,
    table: &str,
    field: &str,
    term: &TokenTermKey,
    cluster_id: u64,
) -> SQLiteResult<Vec<OccurrencePosting>> {
    let stored_cluster = encode_index_u64("posting cluster", cluster_id)?;
    let row: Option<(i64, Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT posting_count, score_blob, positions_blob
               FROM _occurrence_clusters
              WHERE table_name = ?1 AND field = ?2 AND term = ?3
                AND cluster_id = ?4",
            params![table, field, term.as_bytes(), stored_cluster],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_count, score_blob, positions_blob)) = row else {
        return Ok(Vec::new());
    };
    let stored_count = decode_index_u64("posting count", stored_count)?;
    let entries = clustered_result(decode_occurrence_cluster(
        cluster_id,
        &score_blob,
        &positions_blob,
    ))?;
    if stored_count != entries.len() as u64 {
        return Err(SQLiteError::StorageBackend(format!(
            "corrupt clustered posting: stored count {stored_count} disagrees with decoded count {}",
            entries.len()
        )));
    }
    Ok(entries)
}

pub(super) fn write_cluster(
    conn: &rusqlite::Connection,
    table: &str,
    field: &str,
    term: &TokenTermKey,
    cluster_id: u64,
    entries: &[OccurrencePosting],
) -> SQLiteResult<()> {
    let stored_cluster = encode_index_u64("posting cluster", cluster_id)?;
    if entries.is_empty() {
        conn.execute(
            "DELETE FROM _occurrence_clusters
              WHERE table_name = ?1 AND field = ?2 AND term = ?3
                AND cluster_id = ?4",
            params![table, field, term.as_bytes(), stored_cluster],
        )?;
        return Ok(());
    }
    let (score_blob, positions_blob) = clustered_result(encode_occurrence_cluster(entries))?;
    let posting_count = encode_index_counter("posting count", entries.len() as u64)?;
    conn.execute(
        "INSERT INTO _occurrence_clusters
            (table_name, field, term, cluster_id, posting_count,
             score_blob, positions_blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(table_name, field, term, cluster_id) DO UPDATE SET
            posting_count = excluded.posting_count,
            score_blob = excluded.score_blob,
            positions_blob = excluded.positions_blob",
        params![
            table,
            field,
            term.as_bytes(),
            stored_cluster,
            posting_count,
            score_blob,
            positions_blob
        ],
    )?;
    Ok(())
}

pub(super) fn posting_cursor_from_rows(
    rows: Vec<(i64, i64, Vec<u8>)>,
) -> SQLiteResult<Box<dyn PostingCursor>> {
    if rows.is_empty() {
        return Ok(Box::new(clustered_result(MaterializedPostingCursor::new(
            Vec::new(),
        ))?));
    }
    let mut clusters = Vec::with_capacity(rows.len());
    for (cluster_id, stored_count, bytes) in rows {
        let cluster_id = decode_index_u64("posting cluster", cluster_id)?;
        let stored_count = decode_index_u64("posting count", stored_count)?;
        require_score_version(&bytes)?;
        let count = clustered_result(score_count(&bytes))?;
        if count != stored_count {
            return Err(SQLiteError::StorageBackend(
                format!(
                    "corrupt clustered posting: stored count {stored_count} disagrees with score blob count {count}"
                ),
            ));
        }
        clusters.push(EncodedScoreCluster { cluster_id, bytes });
    }
    Ok(Box::new(clustered_result(ClusteredPostingCursor::new(
        clusters,
    ))?))
}

pub(super) fn require_score_version(bytes: &[u8]) -> SQLiteResult<()> {
    if bytes.get(4) != Some(&uqa_storage::clustered_postings::OCCURRENCE_FORMAT_VERSION) {
        return Err(SQLiteError::StorageBackend(
            "occurrence index contains a legacy score payload".into(),
        ));
    }
    Ok(())
}
