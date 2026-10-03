//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph metadata, nodes, and adjacency loading.

use rusqlite::{params, Connection, OptionalExtension};

use super::encoding::{checked_hnsw_level, checked_u64, decode_meta, invalid_metadata, RawMeta};
use super::SQLiteHNSWIndex;
use crate::vector_index::{blob_to_vector, decode_doc_id};
use crate::{Result as SQLiteResult, SQLiteError};
use uqa_core::memory::{Budgeted, MemoryError};
use uqa_storage::hnsw_index::{
    HNSWCanonicalValidator, HNSWGraphMeta, HNSWIndex, HNSWNodeSnapshot, HNSWRestoreBuilder,
};
use uqa_storage::vector_index::HNSWIndexParams;
use uqa_storage::{StorageBackendError, StorageBackendResult};

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

    pub(super) fn load_graph_from(
        &self,
        connection: &Connection,
    ) -> SQLiteResult<(u64, Budgeted<HNSWIndex>)> {
        let Some((dimensions, params, meta, revision)) = load_meta_from(connection, self)? else {
            return Err(super::mutation::missing_metadata(self).into());
        };
        self.validate_header(dimensions, params)?;
        let control = &self.physical_control;
        let mut builder = HNSWRestoreBuilder::new(dimensions, params, meta, control)?;
        load_nodes_into(connection, self, &mut builder)?;
        load_edges_into(connection, self, &mut builder)?;
        let graph = builder.finish()?;
        let mut canonical = HNSWCanonicalValidator::new(&graph, control);
        self.persistent.visit_ordered_vectors_from(
            connection,
            control,
            |document, ordinal, vector| Ok(canonical.push(document, ordinal, vector)?),
        )?;
        canonical.finish()?;
        Ok((revision, graph))
    }

    pub(super) fn load_meta(
        &self,
    ) -> SQLiteResult<Option<(u32, HNSWIndexParams, HNSWGraphMeta, u64)>> {
        if let Some(meta) = self.persistent.read_native(super::native::load_meta)? {
            return Ok(meta);
        }
        self.persistent
            .conn
            .with(|connection| load_meta_from(connection, self))
    }

    pub(super) fn validate_header(
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

pub(super) fn load_meta_from(
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

fn load_nodes_into(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
    builder: &mut HNSWRestoreBuilder,
) -> SQLiteResult<()> {
    let mut statement = connection.prepare(
        "SELECT node_id, doc_id, vector_ordinal, level, deleted, vector
         FROM _hnsw_nodes WHERE table_name = ?1 AND field = ?2 ORDER BY node_id",
    )?;
    let mut rows = statement.query(params![index.persistent.table, index.persistent.field])?;
    while let Some(row) = rows.next()? {
        index.physical_control.check()?;
        let level = checked_hnsw_level("level", row.get(3)?)?;
        let bytes = row
            .get_ref(5)?
            .as_blob()
            .map_err(|_| SQLiteError::StorageBackend("invalid HNSW vector blob".into()))?;
        let size = bytes
            .len()
            .checked_add(size_of::<HNSWNodeSnapshot>())
            .and_then(|size| size.checked_add((level + 1) * size_of::<Vec<u64>>()))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut payload = index.physical_control.memory().reserve(size)?;
        let vector = blob_to_vector(bytes)?;
        let capacity = vector
            .capacity()
            .checked_mul(size_of::<f32>())
            .ok_or(MemoryError::SizeOverflow)?;
        payload.grow(capacity.saturating_sub(bytes.len()))?;
        let ordinal: i64 = row.get(2)?;
        builder.push(HNSWNodeSnapshot {
            node_id: checked_u64("node_id", row.get(0)?)?,
            doc_id: decode_doc_id(row.get(1)?)?,
            vector_ordinal: u32::try_from(ordinal)
                .map_err(|_| invalid_metadata("vector_ordinal", &ordinal.to_string()))?,
            raw_vector: vector,
            level,
            deleted: match row.get::<_, i64>(4)? {
                0 => false,
                1 => true,
                other => return Err(invalid_metadata("deleted", &other.to_string())),
            },
            neighbors: vec![Vec::new(); level + 1],
        })?;
    }
    Ok(())
}

fn load_edges_into(
    connection: &Connection,
    index: &SQLiteHNSWIndex,
    builder: &mut HNSWRestoreBuilder,
) -> SQLiteResult<()> {
    let mut statement = connection.prepare(
        "SELECT source_node_id, layer, target_node_id FROM _hnsw_edges
         WHERE table_name = ?1 AND field = ?2 ORDER BY source_node_id, layer, target_node_id",
    )?;
    let mut rows = statement.query(params![index.persistent.table, index.persistent.field])?;
    while let Some(row) = rows.next()? {
        index.physical_control.check()?;
        builder.edge(
            checked_u64("source_node_id", row.get(0)?)?,
            checked_hnsw_level("layer", row.get(1)?)?,
            checked_u64("target_node_id", row.get(2)?)?,
        )?;
    }
    Ok(())
}
