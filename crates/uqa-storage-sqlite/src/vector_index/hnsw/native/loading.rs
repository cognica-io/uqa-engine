//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode persisted graph records; common storage owns graph validation and reconstruction.

use rusqlite::types::ValueRef;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    hnsw_index::{HNSWCanonicalValidator, HNSWGraphMeta, HNSWIndex, HNSWRestoreBuilder},
    mvcc::HNSWRecordLayout,
    vector_index::HNSWIndexParams,
};

use super::super::encoding::decode_meta;
use super::records::NativeHNSWRecords;
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity};
use crate::vector_index::native::{integer, text, NativeVectorRead};
use crate::Result;

type Meta = (u32, HNSWIndexParams, HNSWGraphMeta, u64);

#[cfg(test)]
thread_local! {
    pub(super) static RESTORED_GRAPHS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

pub(super) fn load_graph(read: &NativeVectorRead<'_>, meta: Meta) -> Result<Budgeted<HNSWIndex>> {
    #[cfg(test)]
    RESTORED_GRAPHS.set(RESTORED_GRAPHS.get() + 1);
    let control = &read.snapshot.control;
    let mut builder = HNSWRestoreBuilder::new(meta.0, meta.1, meta.2, control)?;
    if let Some(owner) = read.owner {
        let nodes = NativeRecordIdentity::new(Family::HNSWNodes, owner)?
            .encode_prefix(&[read.field()], control)?;
        read.snapshot
            .view
            .visit_prefix(&nodes, None, usize::MAX, control, &mut |key, row| {
                if let Some(value) = row.value {
                    let (node, _memory) = NativeHNSWRecords.node(key, value, control)?.into_parts();
                    builder.push(node)?;
                }
                Ok(true)
            })?;
        let edges = NativeRecordIdentity::new(Family::HNSWEdges, owner)?
            .encode_prefix(&[read.field()], control)?;
        read.snapshot
            .view
            .visit_prefix(&edges, None, usize::MAX, control, &mut |key, row| {
                if let Some(value) = row.value {
                    let (source, layer, target) = NativeHNSWRecords.edge(key, value, control)?;
                    builder.edge(source, layer, target)?;
                }
                Ok(true)
            })?;
    }
    let graph = builder.finish()?;
    let mut canonical = HNSWCanonicalValidator::new(&graph, control);
    read.visit_ordered_vectors(|document, ordinal, vector| {
        Ok(canonical.push(document, ordinal, vector)?)
    })?;
    canonical.finish()?;
    Ok(graph)
}
