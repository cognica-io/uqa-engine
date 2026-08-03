//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph metadata, nodes, and adjacency loading.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use super::consistency::validate_canonical_vectors;
use super::encoding::{checked_hnsw_level, checked_u64, decode_meta, invalid_metadata, RawMeta};
use super::SQLiteHNSWIndex;
use crate::hnsw_index::{HNSWGraphMeta, HNSWIndex, HNSWNodeSnapshot};
use crate::sqlite::vector_index::{blob_to_vector, decode_doc_id};
use crate::sqlite::{Result as SQLiteResult, SQLiteError};
use crate::vector_index::HNSWIndexParams;
use crate::{StorageBackendError, StorageBackendResult};

impl SQLiteHNSWIndex {
    pub(crate) fn validate_existing(&self) -> StorageBackendResult<()> {
        let Some((dimensions, params, _, _)) = self.load_meta()? else {
            return Err(StorageBackendError::Other(format!(
                "missing persisted HNSW metadata for {}.{}",
                self.persistent.table, self.persistent.field
            )));
        };
        self.validate_header(dimensions, params)
    }

    pub(super) fn load_graph(&self) -> StorageBackendResult<(u64, HNSWIndex)> {
        let (dimensions, params, meta, revision, nodes) =
            self.persistent.conn.with_mut(|connection| {
                let transaction = connection.savepoint()?;
                let Some((dimensions, params, meta, revision)) =
                    load_meta_from(&transaction, self)?
                else {
                    return Err(SQLiteError::StorageBackend(format!(
                        "missing persisted HNSW metadata for {}.{}",
                        self.persistent.table, self.persistent.field
                    )));
                };
                let mut nodes = load_nodes_from(&transaction, self)?;
                load_edges_into(&transaction, self, &mut nodes)?;
                let canonical = self.persistent.load_all_with_ordinals_from(&transaction)?;
                validate_canonical_vectors(&canonical, &nodes)?;
                transaction.commit()?;
                Ok((dimensions, params, meta, revision, nodes))
            })?;
        self.validate_header(dimensions, params)?;
        Ok((
            revision,
            HNSWIndex::from_persistence(dimensions, params, meta, nodes)?,
        ))
    }

    pub(super) fn load_meta(
        &self,
    ) -> SQLiteResult<Option<(u32, HNSWIndexParams, HNSWGraphMeta, u64)>> {
        self.persistent
            .conn
            .with(|connection| load_meta_from(connection, self))
    }

    fn validate_header(
        &self,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> StorageBackendResult<()> {
        if dimensions != self.persistent.dimensions {
            return Err(StorageBackendError::Other(format!(
                "persisted HNSW dimensions {dimensions} do not match {} for {}.{}",
                self.persistent.dimensions, self.persistent.table, self.persistent.field
            )));
        }
        if params != self.params {
            return Err(StorageBackendError::Other(format!(
                "persisted HNSW parameters do not match the catalog for {}.{}",
                self.persistent.table, self.persistent.field
            )));
        }
        Ok(())
    }
}

fn load_meta_from(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
) -> SQLiteResult<Option<(u32, HNSWIndexParams, HNSWGraphMeta, u64)>> {
    let row = connection
        .query_row(
            "SELECT dimensions, m, ef_construction, ef_search, rebuild_threshold,
                    seed, entry_node_id, max_level, next_node_id, live_count,
                    deleted_count, revision, format_version
               FROM _hnsw_indexes
              WHERE table_name = ?1 AND field = ?2",
            params![index.persistent.table, index.persistent.field],
            |row| -> rusqlite::Result<RawMeta> {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                ))
            },
        )
        .optional()?;
    row.map(decode_meta).transpose()
}

fn load_nodes_from(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
) -> SQLiteResult<Vec<HNSWNodeSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT node_id, doc_id, vector_ordinal, level, deleted, vector
           FROM _hnsw_nodes
          WHERE table_name = ?1 AND field = ?2 ORDER BY node_id",
    )?;
    let rows = statement.query_map(
        params![index.persistent.table, index.persistent.field],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        },
    )?;
    let mut nodes = Vec::new();
    for row in rows {
        let (node_id, doc_id, ordinal, level, deleted, vector) = row?;
        let level = checked_hnsw_level("level", level)?;
        nodes.push(HNSWNodeSnapshot {
            node_id: checked_u64("node_id", node_id)?,
            doc_id: decode_doc_id(doc_id)?,
            vector_ordinal: u32::try_from(ordinal)
                .map_err(|_| invalid_metadata("vector_ordinal", &ordinal.to_string()))?,
            raw_vector: blob_to_vector(&vector)?,
            level,
            deleted: match deleted {
                0 => false,
                1 => true,
                other => return Err(invalid_metadata("deleted", &other.to_string())),
            },
            neighbors: vec![Vec::new(); level + 1],
        });
    }
    Ok(nodes)
}

fn load_edges_into(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
    nodes: &mut [HNSWNodeSnapshot],
) -> SQLiteResult<()> {
    let positions = nodes
        .iter()
        .enumerate()
        .map(|(position, node)| (node.node_id, position))
        .collect::<BTreeMap<_, _>>();
    let mut statement = connection.prepare(
        "SELECT source_node_id, layer, target_node_id FROM _hnsw_edges
          WHERE table_name = ?1 AND field = ?2
          ORDER BY source_node_id, layer, target_node_id",
    )?;
    let rows = statement.query_map(
        params![index.persistent.table, index.persistent.field],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    for row in rows {
        let (source, layer, target) = row?;
        let source = checked_u64("source_node_id", source)?;
        let Some(position) = positions.get(&source).copied() else {
            return Err(SQLiteError::StorageBackend(format!(
                "corrupt HNSW graph: edge source {source} is missing"
            )));
        };
        let layer = checked_hnsw_level("layer", layer)?;
        let node = &mut nodes[position];
        if layer > node.level {
            return Err(SQLiteError::StorageBackend(format!(
                "corrupt HNSW graph: node {source} has an edge at layer {layer} above level {}",
                node.level
            )));
        }
        node.neighbors[layer].push(checked_u64("target_node_id", target)?);
    }
    Ok(())
}
