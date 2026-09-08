//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-neutral fallback for loading a single graph partition.

use std::collections::BTreeMap;

use super::{
    CatalogFacade, GraphSnapshot, GraphVertexRow, StorageBackendError, StorageBackendResult,
};

pub(super) fn load<C: CatalogFacade + ?Sized>(
    catalog: &C,
    name: &str,
) -> StorageBackendResult<Option<GraphSnapshot>> {
    let exists = catalog
        .load_named_graphs()?
        .iter()
        .any(|graph| graph == name);
    let memberships = catalog
        .load_graph_memberships()?
        .into_iter()
        .filter(|(_, _, graph)| graph == name)
        .collect::<Vec<_>>();
    if !exists {
        return if memberships.is_empty() {
            Ok(None)
        } else {
            Err(StorageBackendError::Other(format!(
                "graph membership references unregistered graph `{name}`"
            )))
        };
    }
    let vertices = catalog
        .load_vertices()?
        .into_iter()
        .map(|(vertex_id, label, properties_json)| {
            (
                vertex_id,
                GraphVertexRow {
                    vertex_id,
                    label,
                    properties_json,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let edges = catalog
        .load_edges()?
        .into_iter()
        .map(|edge| (edge.edge_id, edge))
        .collect::<BTreeMap<_, _>>();
    let mut snapshot = GraphSnapshot {
        vertices: Vec::new(),
        edges: Vec::new(),
        label_registry_json: catalog
            .get_metadata(&format!("graph_label_registry::{name}"))?
            .unwrap_or_default(),
    };
    for (kind, id, _) in memberships {
        match kind.as_str() {
            "vertex" => snapshot
                .vertices
                .push(vertices.get(&id).cloned().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "graph `{name}` references missing vertex {id}"
                    ))
                })?),
            "edge" => snapshot.edges.push(edges.get(&id).cloned().ok_or_else(|| {
                StorageBackendError::Other(format!("graph `{name}` references missing edge {id}"))
            })?),
            _ => {
                return Err(StorageBackendError::Other(format!(
                    "graph `{name}` has invalid membership type `{kind}`"
                )))
            }
        }
    }
    Ok(Some(snapshot))
}
