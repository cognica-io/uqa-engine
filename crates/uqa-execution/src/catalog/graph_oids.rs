//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The OIDs graphs record when they and their labels are created, one catalog metadata entry per graph name. Graphs and labels created before OIDs were recorded derive theirs from their names.

use uqa_sql::catalog::graph_oids::{GraphCatalogOids, GraphOidRegistry};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

/// The catalog metadata key prefix of the entries, which the graph's name completes.
pub const GRAPH_CATALOG_OIDS_METADATA_PREFIX: &str = "graph_catalog_oids:";

pub mod changes;

/// The recorded OIDs of the catalog's graphs by graph name.
pub fn load(catalog: &dyn CatalogFacade) -> StorageBackendResult<GraphOidRegistry> {
    let mut registry = GraphOidRegistry::new();
    for (key, value) in catalog.metadata_with_prefix(GRAPH_CATALOG_OIDS_METADATA_PREFIX)? {
        let graph = key
            .strip_prefix(GRAPH_CATALOG_OIDS_METADATA_PREFIX)
            .unwrap_or(&key)
            .to_string();
        registry
            .insert(graph, serde_json::from_str(&value)?)
            .map_err(|error| StorageBackendError::backend("graph OIDs", error))?;
    }
    Ok(registry)
}

/// Record a graph's OIDs, replacing any it recorded before.
pub fn record(
    catalog: &dyn CatalogFacade,
    graph: &str,
    oids: &GraphCatalogOids,
) -> StorageBackendResult<()> {
    catalog.set_metadata(&metadata_key(graph), &serde_json::to_string(oids)?)
}

/// Forget the OIDs of a graph being removed or renamed.
pub fn forget(catalog: &dyn CatalogFacade, graph: &str) -> StorageBackendResult<()> {
    catalog.delete_metadata(&metadata_key(graph))
}

fn metadata_key(graph: &str) -> String {
    format!("{GRAPH_CATALOG_OIDS_METADATA_PREFIX}{graph}")
}
