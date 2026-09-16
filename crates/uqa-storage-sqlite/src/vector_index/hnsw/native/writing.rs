//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stage HNSW headers and incremental graph changes beside their canonical tensor mutation.

use rusqlite::types::ValueRef;
use uqa_storage::{hnsw_index::HNSWPersistenceDelta, KeyValueBatch};

use super::super::{
    encoding::{checked_i64, checked_i64_u64, HNSW_FORMAT_VERSION},
    SQLiteHNSWIndex,
};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::{encode_doc_id, native::NativeVectorRead, vector_to_blob};
use crate::{Result, SQLiteError};

pub(in crate::vector_index::hnsw) fn drop_metadata(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
) -> Result<()> {
    for family in [Family::HNSWEdges, Family::HNSWNodes, Family::HNSWIndexes] {
        read.clear_family(batch, family)?;
    }
    Ok(())
}

pub(super) fn persist_delta(
    read: &NativeVectorRead<'_>,
    batch: &mut dyn KeyValueBatch,
    index: &SQLiteHNSWIndex,
    delta: &HNSWPersistenceDelta,
    revision: u64,
) -> Result<()> {
    let owner = read.owner.ok_or_else(|| {
        SQLiteError::StorageBackend("HNSW publication requires a native table owner".into())
    })?;
    let table = ValueRef::Text(read.index.table.as_bytes());
    let int = ValueRef::Integer;
    let seed = index.params.seed.to_string();
    let meta = delta.meta;
    read.snapshot.put_row(
        batch,
        Family::HNSWIndexes,
        owner,
        &[
            table,
            read.field(),
            int(i64::from(read.index.dimensions)),
            int(checked_i64("m", index.params.m)?),
            int(checked_i64(
                "ef_construction",
                index.params.ef_construction,
            )?),
            int(checked_i64("ef_search", index.params.ef_search)?),
            int(checked_i64(
                "rebuild_threshold",
                index.params.rebuild_threshold,
            )?),
            ValueRef::Text(seed.as_bytes()),
            meta.entry_point
                .map(|id| checked_i64_u64("entry_node_id", id).map(int))
                .transpose()?
                .unwrap_or(ValueRef::Null),
            int(checked_i64("max_level", meta.max_level)?),
            int(checked_i64_u64("next_node_id", meta.next_node_id)?),
            int(checked_i64("live_count", meta.live_count)?),
            int(checked_i64("deleted_count", meta.deleted_count)?),
            int(checked_i64_u64("revision", revision)?),
            int(HNSW_FORMAT_VERSION),
        ],
    )?;
    if delta.full_rewrite {
        read.clear_family(batch, Family::HNSWEdges)?;
        read.clear_family(batch, Family::HNSWNodes)?;
    }
    for node in &delta.nodes {
        read.snapshot.control.check()?;
        let id = int(checked_i64_u64("node_id", node.node_id)?);
        let vector = vector_to_blob(&node.raw_vector)?;
        read.snapshot.put_row(
            batch,
            Family::HNSWNodes,
            owner,
            &[
                table,
                read.field(),
                id,
                int(encode_doc_id(node.doc_id)?),
                int(i64::from(node.vector_ordinal)),
                int(checked_i64("level", node.level)?),
                int(i64::from(node.deleted)),
                ValueRef::Blob(&vector),
            ],
        )?;
        read.snapshot
            .delete_prefix(batch, Family::HNSWEdges, owner, &[read.field(), id])?;
        for (layer, neighbors) in node.neighbors.iter().enumerate() {
            for target in neighbors {
                read.snapshot.put_row(
                    batch,
                    Family::HNSWEdges,
                    owner,
                    &[
                        table,
                        read.field(),
                        id,
                        int(checked_i64("layer", layer)?),
                        int(checked_i64_u64("target_node_id", *target)?),
                    ],
                )?;
            }
        }
    }
    Ok(())
}
