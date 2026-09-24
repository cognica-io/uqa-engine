//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Path definitions, evaluated pairs and private invalidation previews over native records.

use uqa_storage::{mvcc::GraphMutation, KeyValueBatch};

use super::{owner, text, Family, NativeSnapshot, Result, ValueRef};
use crate::mvcc::native::{NativeRecord, NativeRecordIdentity};

pub(in crate::catalog) fn invalidate_graph(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
) -> Result<()> {
    batch.graph_mutation(GraphMutation::InvalidateGraph(graph))?;
    snapshot.visit_paged_rows(
        Family::GraphLookups,
        &[text("path"), text(graph), ValueRef::Integer(0)],
        |entry| {
            snapshot.read_row(
                Family::GraphPathIndexState,
                owner(snapshot),
                &[entry[3]],
                |row| {
                    if row[1] == text(graph) {
                        let record = NativeRecord::encode(
                            Family::GraphPathIndexState,
                            owner(snapshot),
                            &[row[0], row[1], row[2], ValueRef::Integer(0)],
                            &snapshot.control,
                        )?;
                        batch.preview_graph_invalidation(record.key(), Some(record.row()))?;
                    }
                    Ok(())
                },
            )?;
            Ok(true)
        },
    )
}

pub(in crate::catalog) fn clear(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    index: &str,
) -> Result<()> {
    snapshot.delete_prefix(
        batch,
        Family::GraphPathPairs,
        owner(snapshot),
        &[text(index)],
    )?;
    snapshot.replace_graph_row(
        batch,
        Family::GraphPathIndexState,
        &[text(index)],
        None,
        true,
    )
}

pub(in crate::catalog) fn definition(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    index: &str,
    value: Option<&str>,
) -> Result<()> {
    if let Some(value) = value {
        batch.graph_mutation(GraphMutation::InvalidatePath(index))?;
        snapshot.read_row(
            Family::GraphPathIndexState,
            owner(snapshot),
            &[text(index)],
            |row| {
                let record = NativeRecord::encode(
                    Family::GraphPathIndexState,
                    owner(snapshot),
                    &[row[0], row[1], row[2], ValueRef::Integer(0)],
                    &snapshot.control,
                )?;
                batch.preview_graph_invalidation(record.key(), Some(record.row()))?;
                Ok(())
            },
        )?;
        snapshot.put_row(
            batch,
            Family::PathIndexes,
            owner(snapshot),
            &[text(index), text(value)],
        )
    } else {
        clear(snapshot, batch, index)?;
        snapshot.delete_prefix(batch, Family::PathIndexes, owner(snapshot), &[text(index)])
    }
}

pub(in crate::catalog) fn finish(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    index: &str,
    graph: &str,
    definition: &str,
) -> Result<()> {
    let matches = snapshot.read_row(
        Family::PathIndexes,
        owner(snapshot),
        &[text(index)],
        |row| Ok(row[1] == text(definition)),
    )?;
    if matches != Some(true) {
        return Err(crate::SQLiteError::StorageBackend(format!(
            "path index {index:?} definition changed during build"
        )));
    }
    snapshot.replace_graph_row(
        batch,
        Family::GraphPathIndexState,
        &[text(index)],
        Some(&[
            text(index),
            text(graph),
            text(definition),
            ValueRef::Integer(1),
        ]),
        true,
    )?;
    batch.graph_mutation(GraphMutation::PublishPath {
        index,
        graph,
        definition,
    })?;
    Ok(())
}

pub(in crate::catalog) fn current(
    snapshot: &NativeSnapshot,
    index: &str,
    definition: &str,
) -> Result<bool> {
    Ok(snapshot
        .read_row(
            Family::GraphPathIndexState,
            owner(snapshot),
            &[text(index)],
            |row| Ok(row[2] == text(definition) && row[3] == ValueRef::Integer(1)),
        )?
        .unwrap_or(false))
}

pub(in crate::catalog) fn save_pairs(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    index: &str,
    sequence: &str,
    pairs: &[(i64, i64)],
) -> Result<()> {
    for &(source, target) in pairs {
        let row = [
            text(index),
            text(sequence),
            ValueRef::Integer(source),
            ValueRef::Integer(target),
        ];
        if !snapshot.contains_row(Family::GraphPathPairs, owner(snapshot), &row)? {
            snapshot.put_row(batch, Family::GraphPathPairs, owner(snapshot), &row)?;
        }
    }
    Ok(())
}

pub(in crate::catalog) fn pairs(
    snapshot: &NativeSnapshot,
    index: &str,
    sequence: &str,
    after: Option<(u64, u64)>,
    limit: usize,
) -> Result<Vec<(u64, u64)>> {
    let identity = NativeRecordIdentity::new(Family::GraphPathPairs, owner(snapshot))?;
    let prefix = identity.encode_prefix(&[text(index), text(sequence)], &snapshot.control)?;
    let cursor = after
        .map(|(source, target)| {
            identity
                .encode_key(
                    &[
                        text(index),
                        text(sequence),
                        ValueRef::Integer(super::encode_catalog_id("path source", source)?),
                        ValueRef::Integer(super::encode_catalog_id("path target", target)?),
                    ],
                    &snapshot.control,
                )
                .map_err(crate::SQLiteError::from)
        })
        .transpose()?;
    let mut pairs = Vec::new();
    snapshot.view.visit_keys(
        &prefix,
        cursor.as_deref(),
        usize::MAX,
        &snapshot.control,
        &mut |key, record| {
            if record.live {
                let mut pair = [0_u64; 2];
                NativeRecordIdentity::visit_key_components(
                    key,
                    &snapshot.control,
                    |slot, value| {
                        if slot >= 2 {
                            pair[slot - 2] = value
                                .as_i64()
                                .ok()
                                .and_then(|id| u64::try_from(id).ok())
                                .ok_or(uqa_storage::mvcc::VersionError::InvalidEncoding(
                                    "invalid native path pair identity",
                                ))?;
                        }
                        Ok(())
                    },
                )?;
                pairs.push((pair[0], pair[1]));
            }
            Ok(pairs.len() < limit)
        },
    )?;
    Ok(pairs)
}
