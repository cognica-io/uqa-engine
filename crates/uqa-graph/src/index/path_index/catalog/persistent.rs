//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind logical path names to their original physical materializations in one catalog snapshot.

use super::{PathIndex, PathIndexCatalog};
use crate::GraphCatalog;
use std::{collections::BTreeSet, sync::Arc};
use uqa_storage::{
    catalog::path_index, CatalogFacade, PersistentStorageBackend, StorageBackendError,
    StorageBackendResult,
};

impl PathIndexCatalog {
    /// Reconcile cold opens and selected graph refreshes through the same definition and owner checks. Materialized pairs stay in the receiving physical session.
    pub fn restored(
        &self,
        graphs: &GraphCatalog,
        catalog: &Arc<dyn CatalogFacade>,
        backend: &Arc<dyn PersistentStorageBackend>,
        changed: Option<&BTreeSet<String>>,
    ) -> StorageBackendResult<Self> {
        let mut candidate = match changed {
            None => Self::new(self.indexes.budget()),
            Some(names) => {
                let mut next = self.clone();
                for graph in names {
                    next.remove_graph(graph)?;
                }
                next
            }
        };
        for (physical, definition) in catalog.load_path_indexes()? {
            // Cold opening diagnoses the original declaration before its graph. A selective refresh leaves other declarations and materializations untouched.
            let sequences = if let Some(names) = changed {
                if !path_index::may_belong_to(&physical, |graph| names.contains(graph)) {
                    continue;
                }
                None
            } else {
                Some(serde_json::from_str::<Vec<Vec<String>>>(&definition)?)
            };
            let recorded = catalog.path_index_graph(&physical)?;
            let (graph, name) = path_index::resolve(&physical, recorded.as_deref(), |graph| {
                Ok(graphs.contains_key(graph))
            })?
            .ok_or_else(|| {
                StorageBackendError::Other(format!("invalid path-index key `{physical}`"))
            })?;
            if changed.is_some_and(|names| !names.contains(graph.as_ref())) {
                continue;
            }
            let key = path_index::key(&graph, &name);
            if candidate.contains_key(&key) {
                return Err(StorageBackendError::Other(format!(
                    "duplicate path index `{key}`"
                )));
            }
            let sequences = match sequences {
                Some(sequences) => sequences,
                None => serde_json::from_str(&definition)?,
            };
            let index = PathIndex::open_persistent(
                Arc::clone(catalog),
                Arc::clone(backend),
                &physical,
                &graph,
                &sequences,
            )
            .map_err(|error| StorageBackendError::backend("graph catalog", error))?;
            candidate.insert(&key, index)?;
        }
        Ok(candidate)
    }

    /// Remove only registrations of this graph, including encoded names and ordinary keys.
    pub fn remove_graph(&mut self, graph: &str) -> StorageBackendResult<()> {
        let (ordinary, encoded) = path_index::prefixes(graph);
        let mut candidate = self.clone();
        if let Some(prefix) = ordinary {
            candidate.remove_prefix(&prefix)?;
        }
        // A literal graph name may start with another graph's encoded prefix.
        // Its ordinary pair still has a literal separator, excluded from every encoded key.
        candidate.remove_prefix_if(&encoded, |key| !key.contains("::"))?;
        self.publish(candidate);
        Ok(())
    }
}
