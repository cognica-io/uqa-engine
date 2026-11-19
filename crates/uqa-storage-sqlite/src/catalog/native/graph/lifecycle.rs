//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic graph replacement and removal evaluate their selected identities before staging publication.

use super::{
    encode_catalog_id, owner, paths, source, text, write, Family, GraphEntityFilter,
    GraphEntityKind, NativeSnapshot, Result, ValueRef,
};
use crate::mvcc::native::NativeRecordIdentity;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{GraphSnapshot, KeyValueBatch};

fn final_rows<T>(
    snapshot: &NativeSnapshot,
    rows: &[T],
    id: impl Fn(&T) -> u64,
) -> Result<BudgetedVec<(u64, usize)>> {
    let mut order = BudgetedVec::new(snapshot.control.memory());
    for (slot, row) in rows.iter().enumerate() {
        snapshot.control.check()?;
        let id = id(row);
        encode_catalog_id("graph entity", id)?;
        order.push((id, slot))?;
    }
    order.sort_unstable();
    let mut length = 0;
    for read in 0..order.len() {
        if read + 1 == order.len() || order[read].0 != order[read + 1].0 {
            order[length] = order[read];
            length += 1;
        }
    }
    order.truncate(length);
    Ok(order)
}

pub(in crate::catalog) fn detach(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
) -> Result<()> {
    snapshot.fence_graph_definition(batch, None, graph)?;
    let registration = NativeRecordIdentity::new(Family::NamedGraphs, owner(snapshot))?
        .encode_key(&[text(graph)], &snapshot.control)?;
    batch.fence_record(&registration)?;
    write::remove_memberships(snapshot, batch, graph, |_, _| false)
}

pub(in crate::catalog) fn purge(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    removed_graph: Option<&str>,
    keep: impl Fn(GraphEntityKind, u64) -> bool,
) -> Result<()> {
    for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
        snapshot.visit_graph_ids(None, GraphEntityFilter::new(kind, None), None, |id| {
            let decoded = super::decode_catalog_id("graph entity", id)?;
            let mut retained = keep(kind, decoded);
            if !retained {
                snapshot.visit_paged_rows(
                    Family::GraphMembership,
                    &[text(kind.as_str()), ValueRef::Integer(id)],
                    |row| {
                        let graph = row[2].as_str().map_err(|_| {
                            crate::SQLiteError::StorageBackend(
                                "invalid graph membership name".into(),
                            )
                        })?;
                        retained = Some(graph) != removed_graph;
                        Ok(!retained)
                    },
                )?;
            }
            if !retained {
                source(snapshot, batch, kind, id, None)?;
            }
            Ok(true)
        })?;
    }
    Ok(())
}

pub(in crate::catalog) fn remove_paths(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
) -> Result<()> {
    let prefix = NativeRecordIdentity::new(Family::PathIndexes, owner(snapshot))?
        .encode_prefix(&[], &snapshot.control)?;
    visit_path_keys(snapshot, &prefix, |_, name| {
        if uqa_storage::catalog::path_index::belongs_to(
            name,
            graph,
            || paths::recorded_graph(snapshot, name).map_err(Into::into),
            |candidate| super::named_graph_exists(snapshot, candidate).map_err(Into::into),
        )? {
            paths::definition(snapshot, batch, name, None)?;
        }
        Ok(())
    })
}

pub(super) fn retain_path_owners(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
) -> Result<()> {
    let mut legacy_prefix =
        crate::read_control::copy_bytes(graph.as_bytes(), 2, &snapshot.control)?;
    legacy_prefix.extend_from_slice(b"::")?;
    let prefix = NativeRecordIdentity::new(Family::PathIndexes, owner(snapshot))?
        .encode_text_prefix(&legacy_prefix, &snapshot.control)?;
    visit_path_keys(snapshot, &prefix, |key, name| {
        if let Some(previous) = uqa_storage::catalog::path_index::owner_before_graph_creation(
            name,
            graph,
            || {
                snapshot
                    .contains_row(Family::GraphPathIndexState, owner(snapshot), &[text(name)])
                    .map_err(Into::into)
            },
            |candidate| super::named_graph_exists(snapshot, candidate).map_err(Into::into),
        )? {
            batch.require_unchanged(key)?;
            let registration = NativeRecordIdentity::new(Family::NamedGraphs, owner(snapshot))?
                .encode_key(&[text(previous)], &snapshot.control)?;
            batch.require_unchanged(&registration)?;
            snapshot.replace_graph_row(
                batch,
                Family::GraphPathIndexState,
                &[text(name)],
                Some(&[text(name), text(previous), text("[]"), ValueRef::Integer(0)]),
                false,
            )?;
        }
        Ok(())
    })
}

fn visit_path_keys(
    snapshot: &NativeSnapshot,
    prefix: &[u8],
    mut visit: impl FnMut(&[u8], &str) -> Result<()>,
) -> Result<()> {
    let mut after = BudgetedVec::new(snapshot.control.memory());
    loop {
        let mut keys = BudgetedVec::new(snapshot.control.memory());
        let mut last = BudgetedVec::new(snapshot.control.memory());
        snapshot.view.visit_keys(
            prefix,
            (!after.is_empty()).then_some(&*after),
            64,
            &snapshot.control,
            &mut |key, record| {
                last.clear();
                last.extend_from_slice(key)?;
                if record.live {
                    let mut owned = BudgetedVec::new(snapshot.control.memory());
                    owned.extend_from_slice(key)?;
                    keys.push(owned)?;
                }
                Ok(true)
            },
        )?;
        if last.is_empty() {
            break;
        }
        after = last;
        for key in keys.iter() {
            NativeRecordIdentity::visit_key_components(key, &snapshot.control, |_, value| {
                let name = value.as_str().map_err(|_| {
                    uqa_storage::mvcc::VersionError::InvalidEncoding(
                        "native path definition identity is not text",
                    )
                })?;
                visit(key, name)
                    .map_err(|error| uqa_storage::mvcc::VersionError::Storage(error.into()))
            })?;
        }
    }
    Ok(())
}

pub(in crate::catalog) fn replace(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
    replacement: &GraphSnapshot,
) -> Result<()> {
    let vertices = final_rows(snapshot, &replacement.vertices, |row| row.vertex_id)?;
    snapshot.fence_graph_definition(batch, None, graph)?;
    snapshot.observe_graph_registry(batch, None, graph, &replacement.label_registry_json)?;
    let edges = final_rows(snapshot, &replacement.edges, |row| row.edge_id)?;
    for edge in &replacement.edges {
        encode_catalog_id("edge source", edge.source_id)?;
        encode_catalog_id("edge target", edge.target_id)?;
    }
    let keep = |kind, id| {
        let selected = if kind == GraphEntityKind::Vertex {
            &vertices
        } else {
            &edges
        };
        selected.binary_search_by_key(&id, |entry| entry.0).is_ok()
    };
    write::named_graph(snapshot, batch, graph, true)?;
    paths::invalidate_graph(snapshot, batch, graph)?;
    write::remove_memberships(snapshot, batch, graph, |kind, id| match kind {
        "vertex" => keep(GraphEntityKind::Vertex, id),
        "edge" => keep(GraphEntityKind::Edge, id),
        _ => false,
    })?;
    for &(id, slot) in vertices.iter() {
        let row = &replacement.vertices[slot];
        let id = encode_catalog_id("vertex", id)?;
        source(
            snapshot,
            batch,
            GraphEntityKind::Vertex,
            id,
            Some(&[
                ValueRef::Integer(id),
                text(&row.label),
                text(&row.properties_json),
            ]),
        )?;
        write::membership_with_entity(
            snapshot,
            batch,
            "vertex",
            id,
            graph,
            true,
            Some(
                uqa_storage::catalog::graph_observations::GraphEntityTopology::Vertex {
                    label: &row.label,
                },
            ),
        )?;
    }
    for &(id, slot) in edges.iter() {
        let row = &replacement.edges[slot];
        let id = encode_catalog_id("edge", id)?;
        source(
            snapshot,
            batch,
            GraphEntityKind::Edge,
            id,
            Some(&[
                ValueRef::Integer(id),
                ValueRef::Integer(encode_catalog_id("edge source", row.source_id)?),
                ValueRef::Integer(encode_catalog_id("edge target", row.target_id)?),
                text(&row.label),
                text(&row.properties_json),
            ]),
        )?;
        write::membership_with_entity(
            snapshot,
            batch,
            "edge",
            id,
            graph,
            true,
            Some(
                uqa_storage::catalog::graph_observations::GraphEntityTopology::Edge {
                    label: &row.label,
                    source: row.source_id,
                    target: row.target_id,
                },
            ),
        )?;
    }
    let mut metadata = BudgetedVec::new(snapshot.control.memory());
    metadata.extend_from_slice(b"graph_label_registry::")?;
    metadata.extend_from_slice(graph.as_bytes())?;
    snapshot.put_row(
        batch,
        Family::Metadata,
        owner(snapshot),
        &[
            ValueRef::Text(&metadata),
            text(&replacement.label_registry_json),
        ],
    )?;
    purge(snapshot, batch, Some(graph), keep)?;
    remove_paths(snapshot, batch, graph)
}

pub(in crate::catalog) fn drop_data(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    graph: &str,
) -> Result<()> {
    write::named_graph(snapshot, batch, graph, false)?;
    let mut metadata = BudgetedVec::new(snapshot.control.memory());
    metadata.extend_from_slice(b"graph_label_registry::")?;
    metadata.extend_from_slice(graph.as_bytes())?;
    snapshot.delete_prefix(
        batch,
        Family::Metadata,
        owner(snapshot),
        &[ValueRef::Text(&metadata)],
    )?;
    purge(snapshot, batch, Some(graph), |_, _| false)?;
    remove_paths(snapshot, batch, graph)
}
