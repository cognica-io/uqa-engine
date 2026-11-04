//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph identity changes prepare complete native successors before persisting their original record order.

use crate::catalog::identity::{
    allocate_catalog_object_id, CatalogIdentityReservationContext, LabelShape,
};
use std::collections::BTreeSet;
use uqa_graph::{GraphStore as _, GraphStoreHandle, LabelKind};
use uqa_sql::{catalog::graph_oids::GraphOidRegistry, SQLError};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub struct GraphOidChange {
    next: GraphOidRegistry,
    removed: Option<String>,
    recorded: Vec<String>,
}

impl GraphOidChange {
    /// The enclosing graph transaction owns storage rollback; Engine installs the returned root only after every record succeeds.
    pub fn persist(
        self,
        catalog: Option<&dyn CatalogFacade>,
    ) -> StorageBackendResult<GraphOidRegistry> {
        if let Some(catalog) = catalog {
            if let Some(name) = &self.removed {
                super::forget(catalog, name)?;
            }
            for name in &self.recorded {
                super::record(catalog, name, &self.next[name])?;
            }
        }
        Ok(self.next)
    }
}

fn identity_error(error: SQLError) -> StorageBackendError {
    StorageBackendError::backend("graph OIDs", error)
}

/// Retain legacy graphs without records, allocate new graph objects first, then allocate missing labels in the store's existing order.
pub fn created(
    current: &GraphOidRegistry,
    published: &BTreeSet<String>,
    candidate: &GraphStoreHandle,
    reservation: CatalogIdentityReservationContext<'_>,
    namespace_in_use: impl Fn(i64) -> Result<bool, SQLError>,
) -> StorageBackendResult<Option<GraphOidChange>> {
    let mut next = current.clone();
    let mut changed = BTreeSet::new();
    for graph in candidate
        .graph_names()
        .map_err(|error| StorageBackendError::backend("graph", error))?
    {
        let mut selected = if !published.contains(&graph) && !next.contains_key(&graph) {
            let oids = reservation
                .allocator(allocate_catalog_object_id)
                .allocate_graph_oids(&graph, &namespace_in_use)
                .map_err(identity_error)?;
            changed.insert(graph.clone());
            Some(oids)
        } else {
            None
        };
        let original = next.get(&graph);
        if selected.is_none() && original.is_none() {
            continue;
        }
        for label in candidate
            .graph_labels(&graph)
            .map_err(|error| StorageBackendError::backend("graph", error))?
        {
            let existing = selected
                .as_ref()
                .or(original)
                .expect("selected recorded graph");
            if existing.labels.contains_key(&label.id) {
                continue;
            }
            let oids = reservation
                .allocator(allocate_catalog_object_id)
                .allocate_label_oids(
                    &graph,
                    &label.name,
                    LabelShape {
                        edge: label.kind == LabelKind::Edge,
                        default: false,
                    },
                )
                .map_err(|error| StorageBackendError::backend("label OIDs", error))?;
            selected
                .get_or_insert_with(|| original.expect("existing graph generation").clone())
                .labels
                .insert(label.id, oids);
            changed.insert(graph.clone());
        }
        if let Some(oids) = selected {
            next.insert(graph, oids).map_err(identity_error)?;
        }
    }
    Ok((!changed.is_empty()).then(|| GraphOidChange {
        next,
        removed: None,
        recorded: changed.into_iter().collect(),
    }))
}

pub fn labels_removed(
    current: &GraphOidRegistry,
    graph: &str,
    label_ids: &[u32],
) -> StorageBackendResult<Option<GraphOidChange>> {
    let mut next = current.clone();
    if next
        .edit(graph, |oids| {
            for id in label_ids {
                oids.labels.remove(id);
            }
            Ok(())
        })
        .map_err(identity_error)?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(GraphOidChange {
        next,
        removed: None,
        recorded: vec![graph.into()],
    }))
}

pub fn removed(
    current: &GraphOidRegistry,
    graph: &str,
) -> StorageBackendResult<Option<GraphOidChange>> {
    let mut next = current.clone();
    if !next.remove(graph).map_err(identity_error)? {
        return Ok(None);
    }
    Ok(Some(GraphOidChange {
        next,
        removed: Some(graph.into()),
        recorded: Vec::new(),
    }))
}

pub fn renamed(
    current: &GraphOidRegistry,
    from: &str,
    to: &str,
) -> StorageBackendResult<Option<GraphOidChange>> {
    let Some(oids) = current.get(from).cloned() else {
        return Ok(None);
    };
    let mut next = current.clone();
    next.remove(from).map_err(identity_error)?;
    next.insert(to.into(), oids).map_err(identity_error)?;
    Ok(Some(GraphOidChange {
        next,
        removed: Some(from.into()),
        recorded: vec![to.into()],
    }))
}

#[cfg(test)]
mod tests;
