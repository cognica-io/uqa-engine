//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode persisted graph records; common storage owns graph validation and reconstruction.

use rusqlite::types::ValueRef;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError, MemoryReservation};
use uqa_storage::{
    hnsw_index::{HNSWGraphMeta, HNSWIndex, HNSWNodeSnapshot},
    vector_index::HNSWIndexParams,
};

use super::super::{
    consistency::validate_canonical_vectors,
    encoding::{checked_hnsw_level, checked_u64, decode_meta, invalid_metadata},
};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::{
    blob_to_vector, decode_doc_id,
    native::{blob, integer, text, NativeVectorRead, VectorBuffer},
};
use crate::{Result, SQLiteError};

type Meta = (u32, HNSWIndexParams, HNSWGraphMeta, u64);

pub(in crate::vector_index::hnsw) fn load_meta(
    read: &NativeVectorRead<'_>,
) -> Result<Option<Meta>> {
    let Some(owner) = read.owner else {
        return Ok(None);
    };
    read.snapshot
        .read_row(Family::HNSWIndexes, owner, &[read.field()], |row| {
            decode_meta((
                integer(row[2])?,
                integer(row[3])?,
                integer(row[4])?,
                integer(row[5])?,
                integer(row[6])?,
                text(row[7])?,
                if row[8] == ValueRef::Null {
                    None
                } else {
                    Some(integer(row[8])?)
                },
                integer(row[9])?,
                integer(row[10])?,
                integer(row[11])?,
                integer(row[12])?,
                integer(row[13])?,
                integer(row[14])?,
            ))
        })
}

pub(super) fn load_graph(read: &NativeVectorRead<'_>, meta: Meta) -> Result<HNSWIndex> {
    let nodes = load_nodes(read)?;
    let canonical = read.vectors()?;
    validate_canonical_vectors(&canonical, &nodes)?;
    // Keep decoded-buffer reservations until common reconstruction has consumed their allocations.
    let (nodes, _decoded) = nodes.into_parts();
    Ok(HNSWIndex::from_persistence(meta.0, meta.1, meta.2, nodes)?)
}

fn load_nodes(read: &NativeVectorRead<'_>) -> Result<Budgeted<Vec<HNSWNodeSnapshot>>> {
    let mut output = VectorBuffer::new(read)?;
    let Some(owner) = read.owner else {
        return Ok(output.finish());
    };
    let (nodes, payload) = (&mut output.rows, &mut output.payload);
    read.snapshot
        .visit_rows(Family::HNSWNodes, Some(owner), &[read.field()], |row| {
            let level = checked_hnsw_level("level", integer(row[5])?)?;
            let raw = blob(row[7])?;
            let layers = (level + 1)
                .checked_mul(std::mem::size_of::<Vec<u64>>())
                .ok_or(MemoryError::SizeOverflow)?;
            payload.grow(
                raw.len()
                    .checked_add(layers)
                    .ok_or(MemoryError::SizeOverflow)?,
            )?;
            nodes.reserve(1)?;
            let vector = blob_to_vector(raw)?;
            read.index.validate_dimensions_sqlite(&vector)?;
            let ordinal = integer(row[4])?;
            nodes.push(HNSWNodeSnapshot {
                node_id: checked_u64("node_id", integer(row[2])?)?,
                doc_id: decode_doc_id(integer(row[3])?)?,
                vector_ordinal: u32::try_from(ordinal)
                    .map_err(|_| invalid_metadata("vector_ordinal", &ordinal.to_string()))?,
                raw_vector: vector,
                level,
                deleted: match integer(row[6])? {
                    0 => false,
                    1 => true,
                    other => return Err(invalid_metadata("deleted", &other.to_string())),
                },
                neighbors: vec![Vec::new(); level + 1],
            })?;
            Ok(())
        })?;
    read.snapshot
        .visit_rows(Family::HNSWEdges, Some(owner), &[read.field()], |row| {
            let source = checked_u64("source_node_id", integer(row[2])?)?;
            let position = nodes
                .binary_search_by_key(&source, |node| node.node_id)
                .map_err(|_| {
                    SQLiteError::StorageBackend(format!(
                        "corrupt HNSW graph: edge source {source} is missing"
                    ))
                })?;
            let layer = checked_hnsw_level("layer", integer(row[3])?)?;
            let node = &mut nodes[position];
            let neighbors = node.neighbors.get_mut(layer).ok_or_else(|| {
                SQLiteError::StorageBackend(format!(
                    "corrupt HNSW graph: node {source} has an edge above its level"
                ))
            })?;
            append_neighbor(
                neighbors,
                payload,
                checked_u64("target_node_id", integer(row[4])?)?,
            )?;
            Ok(())
        })?;
    Ok(output.finish())
}

fn append_neighbor(
    neighbors: &mut Vec<u64>,
    payload: &mut MemoryReservation,
    target: u64,
) -> Result<()> {
    if neighbors.len() == neighbors.capacity() {
        let old_bytes = neighbors
            .capacity()
            .checked_mul(std::mem::size_of::<u64>())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut replacement = BudgetedVec::new(payload.budget());
        replacement.reserve(
            neighbors
                .len()
                .checked_add(1)
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        replacement.extend_from_slice(neighbors)?;
        let (values, memory) = replacement.into_parts();
        // Charge both buffers until the old allocation is freed, including spare capacity.
        drop(std::mem::replace(neighbors, values));
        drop(payload.split(old_bytes));
        payload.absorb(memory);
    }
    neighbors.push(target);
    Ok(())
}
