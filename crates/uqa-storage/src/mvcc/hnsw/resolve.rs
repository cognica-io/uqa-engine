//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reassign shared HNSW node identities and adjacency from the latest validated graph.

mod load;
use crate::mvcc::vector::{
    resolve::{bytes, replace},
    Mutation,
};
use crate::mvcc::{
    CommittedRecordSnapshot, HNSWRecordKey as Key, HNSWRecordLayout, HNSWRecordValue as Value,
    PrivateRecordChanges, RecordWrite, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

pub(in crate::mvcc) fn merge(
    key: &[u8],
    operations: &[Mutation<'_>],
    changes: &PrivateRecordChanges,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn HNSWRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let row = current.get(key, control)?;
    let template = bytes(row.as_ref()).ok_or(VersionError::InvalidEncoding(
        "missing current HNSW definition",
    ))?;
    let header = layout.header(key, template, control)?;
    let index = load::index(key, header, current, layout, control)?;
    let mut inputs = BudgetedVec::new(control.memory());
    for operation in operations {
        control.cancellation().check()?;
        inputs.push(operation.hnsw())?;
    }
    let delta = index.prepare_delta_changes(&inputs, control)?;
    let revision = header
        .revision
        .map(|revision| {
            revision
                .checked_add(operations.len() as u64)
                .ok_or(VersionError::InvalidEncoding("HNSW revision exhausted"))
        })
        .transpose()?;
    if delta.full_rewrite {
        delete_prefix(
            changes,
            current,
            &layout.key(key, Key::Nodes, control)?,
            control,
        )?;
        if let Some(edges) = layout.edges_prefix(key, None, control)? {
            delete_prefix(changes, current, &edges, control)?;
        }
    }
    let value = layout.encode(
        key,
        template,
        Value::Header {
            meta: delta.meta,
            revision,
        },
        control,
    )?;
    replace(changes, current, key, &value, control)?;
    for node in &delta.nodes {
        control.cancellation().check()?;
        let address = layout.key(key, Key::Node(node.node_id), control)?;
        let value = layout.encode(&address, template, Value::Node(node), control)?;
        replace(changes, current, &address, &value, control)?;
        if let Some(edges) = layout.edges_prefix(key, Some(node.node_id), control)? {
            if !delta.full_rewrite {
                delete_prefix(changes, current, &edges, control)?;
            }
            for (layer, neighbors) in node.neighbors.iter().enumerate() {
                for target in neighbors {
                    control.cancellation().check()?;
                    let address = layout.key(
                        key,
                        Key::Edge {
                            source: node.node_id,
                            layer,
                            target: *target,
                        },
                        control,
                    )?;
                    let value = layout.encode(&address, template, Value::Edge, control)?;
                    replace(changes, current, &address, &value, control)?;
                }
            }
        }
    }
    Ok(())
}

fn delete_prefix(
    changes: &PrivateRecordChanges,
    current: &dyn CommittedRecordSnapshot,
    prefix: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    current.visit_keys(prefix, None, usize::MAX, control, &mut |key, row| {
        if row.live {
            changes.apply(
                &[RecordWrite {
                    key,
                    expected: row.revision,
                    value: None,
                }],
                control,
            )?;
        }
        Ok(true)
    })?;
    Ok(())
}
