//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream the current graph and canonical tensors on one committed boundary.

use crate::mvcc::{
    CommittedRecordSnapshot, HNSWRecordHeader, HNSWRecordKey as Key, HNSWRecordLayout,
    VersionResult,
};
use crate::{
    hnsw_index::{HNSWCanonicalValidator, HNSWIndex, HNSWRestoreBuilder},
    read_control::StorageReadControl,
};
use uqa_core::memory::Budgeted;

pub(super) fn index(
    key: &[u8],
    header: HNSWRecordHeader,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn HNSWRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<Budgeted<HNSWIndex>> {
    let mut builder =
        HNSWRestoreBuilder::new(header.dimensions, header.params, header.meta, control)?;
    current.visit_prefix(
        &layout.key(key, Key::Nodes, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (node, _memory) = layout.node(key, value, control)?.into_parts();
                builder.push(node)?;
            }
            Ok(true)
        },
    )?;
    if let Some(edges) = layout.edges_prefix(key, None, control)? {
        current.visit_prefix(&edges, None, usize::MAX, control, &mut |key, row| {
            if let Some(value) = row.value {
                let (source, layer, target) = layout.edge(key, value, control)?;
                builder.edge(source, layer, target)?;
            }
            Ok(true)
        })?;
    }
    let graph = builder.finish()?;
    let mut canonical = HNSWCanonicalValidator::new(&graph, control);
    current.visit_prefix(
        &layout.key(key, Key::Vectors, control)?,
        None,
        usize::MAX,
        control,
        &mut |key, row| {
            if let Some(value) = row.value {
                let (document, ordinal, vector) = layout.vector(key, value, control)?;
                canonical.push(document, ordinal, &vector)?;
            }
            Ok(true)
        },
    )?;
    canonical.finish()?;
    Ok(graph)
}
