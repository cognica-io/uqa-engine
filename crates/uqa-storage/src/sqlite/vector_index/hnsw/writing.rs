//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optimistic revision checks and incremental HNSW graph writes.

use rusqlite::{params, OptionalExtension};

use super::encoding::{
    checked_i64, checked_i64_u64, checked_u64, invalid_metadata, HNSW_FORMAT_VERSION,
};
use super::SQLiteHNSWIndex;
use crate::hnsw_index::{HNSWGraphMeta, HNSWNodeSnapshot, HNSWPersistenceDelta};
use crate::sqlite::vector_index::vector_to_blob;
use crate::sqlite::{ManagedConnection, Result as SQLiteResult, SQLiteError};

impl SQLiteHNSWIndex {
    pub(super) fn persist_delta(
        &self,
        conn: &rusqlite::Connection,
        delta: &HNSWPersistenceDelta,
        expected_revision: Option<u64>,
        revision: u64,
    ) -> SQLiteResult<()> {
        assert_revision(conn, self, expected_revision)?;
        write_meta(conn, self, delta.meta, revision)?;
        if delta.full_rewrite {
            conn.execute(
                "DELETE FROM _hnsw_edges WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            conn.execute(
                "DELETE FROM _hnsw_nodes WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
        }
        for node in &delta.nodes {
            write_node(conn, self, node)?;
        }
        Ok(())
    }
}

pub(super) fn drop_metadata(
    conn: &ManagedConnection,
    table: &str,
    field: &str,
) -> SQLiteResult<()> {
    conn.with_mut(|conn| {
        let tx = conn.savepoint()?;
        for metadata_table in ["_hnsw_edges", "_hnsw_nodes", "_hnsw_indexes"] {
            tx.execute(
                &format!("DELETE FROM {metadata_table} WHERE table_name = ?1 AND field = ?2"),
                params![table, field],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

fn write_meta(
    conn: &rusqlite::Connection,
    index: &SQLiteHNSWIndex,
    meta: HNSWGraphMeta,
    revision: u64,
) -> SQLiteResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _hnsw_indexes
            (table_name, field, dimensions, m, ef_construction, ef_search,
             rebuild_threshold, seed, entry_node_id, max_level, next_node_id,
             live_count, deleted_count, revision, format_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            index.persistent.table,
            index.persistent.field,
            i64::from(index.persistent.dimensions),
            checked_i64("m", index.params.m)?,
            checked_i64("ef_construction", index.params.ef_construction)?,
            checked_i64("ef_search", index.params.ef_search)?,
            checked_i64("rebuild_threshold", index.params.rebuild_threshold)?,
            index.params.seed.to_string(),
            meta.entry_point
                .map(|value| checked_i64_u64("entry_node_id", value))
                .transpose()?,
            checked_i64("max_level", meta.max_level)?,
            checked_i64_u64("next_node_id", meta.next_node_id)?,
            checked_i64("live_count", meta.live_count)?,
            checked_i64("deleted_count", meta.deleted_count)?,
            checked_i64_u64("revision", revision)?,
            HNSW_FORMAT_VERSION,
        ],
    )?;
    Ok(())
}

fn write_node(
    conn: &rusqlite::Connection,
    index: &SQLiteHNSWIndex,
    node: &HNSWNodeSnapshot,
) -> SQLiteResult<()> {
    let node_id = checked_i64_u64("node_id", node.node_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO _hnsw_nodes
            (table_name, field, node_id, doc_id, vector_ordinal, level, deleted, vector)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            index.persistent.table,
            index.persistent.field,
            node_id,
            i64::try_from(node.doc_id)
                .map_err(|_| invalid_metadata("doc_id", &node.doc_id.to_string()))?,
            i64::from(node.vector_ordinal),
            checked_i64("level", node.level)?,
            i64::from(node.deleted),
            vector_to_blob(&node.raw_vector)?,
        ],
    )?;
    conn.execute(
        "DELETE FROM _hnsw_edges
          WHERE table_name = ?1 AND field = ?2 AND source_node_id = ?3",
        params![index.persistent.table, index.persistent.field, node_id],
    )?;
    let mut statement = conn.prepare(
        "INSERT INTO _hnsw_edges
            (table_name, field, source_node_id, layer, target_node_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (layer, neighbors) in node.neighbors.iter().enumerate() {
        for neighbor in neighbors {
            statement.execute(params![
                index.persistent.table,
                index.persistent.field,
                node_id,
                checked_i64("layer", layer)?,
                checked_i64_u64("target_node_id", *neighbor)?,
            ])?;
        }
    }
    Ok(())
}

fn assert_revision(
    conn: &rusqlite::Connection,
    index: &SQLiteHNSWIndex,
    expected: Option<u64>,
) -> SQLiteResult<()> {
    let actual = conn
        .query_row(
            "SELECT revision FROM _hnsw_indexes
              WHERE table_name = ?1 AND field = ?2",
            params![index.persistent.table, index.persistent.field],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|value| checked_u64("revision", value))
        .transpose()?;
    if actual != expected {
        return Err(SQLiteError::StorageBackend(format!(
            "concurrent HNSW metadata change for {}.{}: expected revision {expected:?}, found {actual:?}",
            index.persistent.table, index.persistent.field
        )));
    }
    Ok(())
}
