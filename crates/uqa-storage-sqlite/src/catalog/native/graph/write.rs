//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native source changes share selector and derived-cache effects in one evaluated batch.

use super::{
    entity_family, owner, paths, text, Family, GraphEntityKind, NativeSnapshot, Result, ValueRef,
};
use uqa_storage::{mvcc::GraphMutation, KeyValueBatch};

pub(in crate::catalog) fn source(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    kind: GraphEntityKind,
    id: i64,
    row: Option<&[ValueRef<'_>]>,
) -> Result<()> {
    if row.is_none()
        && !snapshot.contains_row(
            entity_family(kind),
            owner(snapshot),
            &[ValueRef::Integer(id)],
        )?
    {
        return Ok(());
    }
    let id = super::decode_catalog_id("graph entity", id)?;
    batch.graph_mutation(GraphMutation::InvalidateEntity(kind, id))?;
    snapshot.visit_paged_rows(
        Family::GraphMembership,
        &[
            text(kind.as_str()),
            ValueRef::Integer(super::encode_catalog_id("graph entity", id)?),
        ],
        |row| {
            let graph = row[2].as_str().map_err(|_| {
                crate::SQLiteError::StorageBackend("invalid graph membership name".into())
            })?;
            paths::invalidate_graph(snapshot, batch, graph)?;
            Ok(true)
        },
    )?;
    snapshot.replace_graph_row(
        batch,
        entity_family(kind),
        &[ValueRef::Integer(super::encode_catalog_id(
            "graph entity",
            id,
        )?)],
        row,
        false,
    )
}

pub(in crate::catalog) fn membership(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    kind: &str,
    id: i64,
    graph: &str,
    present: bool,
) -> Result<()> {
    let row = [text(kind), ValueRef::Integer(id), text(graph)];
    if snapshot.contains_row(Family::GraphMembership, owner(snapshot), &row)? == present {
        return Ok(());
    }
    paths::invalidate_graph(snapshot, batch, graph)?;
    snapshot.replace_graph_row(
        batch,
        Family::GraphMembership,
        &row,
        present.then_some(&row),
        false,
    )
}

pub(in crate::catalog) fn named_graph(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    present: bool,
) -> Result<()> {
    if !present {
        remove_memberships(snapshot, batch, name, |_, _| false)?;
    }
    if snapshot.contains_row(Family::NamedGraphs, owner(snapshot), &[text(name)])? != present {
        paths::invalidate_graph(snapshot, batch, name)?;
        if present {
            snapshot.put_row(batch, Family::NamedGraphs, owner(snapshot), &[text(name)])?;
        } else {
            snapshot.delete_prefix(batch, Family::NamedGraphs, owner(snapshot), &[text(name)])?;
        }
    }
    Ok(())
}

pub(super) fn remove_memberships(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    name: &str,
    mut keep: impl FnMut(&str, u64) -> bool,
) -> Result<()> {
    snapshot.visit_paged_rows(
        Family::GraphLookups,
        &[text("member"), text(name), ValueRef::Integer(0)],
        |row| {
            let kind = row[3].as_str().map_err(|_| {
                crate::SQLiteError::StorageBackend("invalid graph membership kind".into())
            })?;
            let id = super::integer(row[4])?;
            if keep(kind, super::decode_catalog_id("graph entity", id)?) {
                return Ok(true);
            }
            membership(snapshot, batch, kind, id, name, false)?;
            Ok(true)
        },
    )
}
