//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reachability indexes: primary memory for memory graphs, physical records
//! for durable graphs. No persistent index is rebuilt or hydrated on open.

use crate::{Direction, GraphStore, GraphStoreError, GraphStoreResult, PersistentGraphStore};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uqa_storage::{CatalogFacade, PersistentStorageBackend};

type Pairs = BTreeSet<(u64, u64)>;
type MemoryPaths = BTreeMap<Vec<String>, Pairs>;

#[derive(Clone)]
enum PathStorage {
    Memory(Arc<MemoryPaths>),
    Persistent(Arc<DurablePathIndex>),
    ReadView {
        store: Arc<crate::GraphStoreHandle>,
        graph: String,
        sequences: Vec<Vec<String>>,
    },
}

struct DurablePathIndex {
    catalog: Arc<dyn CatalogFacade>,
    backend: Arc<dyn PersistentStorageBackend>,
    key: String,
    graph: String,
    sequences: Vec<Vec<String>>,
    definition: String,
    read_gate: parking_lot::ReentrantMutex<()>,
}

/// Fixed-label-sequence reachability. Durable clones copy only a handle.
/// Queries return owned results and can report storage errors.
#[derive(Clone)]
pub struct PathIndex {
    storage: PathStorage,
}

impl std::fmt::Debug for PathIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PathIndex")
            .field("paths", &self.indexed_paths())
            .field(
                "persistent",
                &matches!(self.storage, PathStorage::Persistent(_)),
            )
            .finish()
    }
}

impl Default for PathIndex {
    fn default() -> Self {
        Self {
            storage: PathStorage::Memory(Arc::default()),
        }
    }
}

fn json_error(error: &serde_json::Error) -> GraphStoreError {
    GraphStoreError::CorruptGraph(error.to_string())
}

impl PathIndex {
    pub fn build<G: GraphStore>(
        store: &G,
        graph: &str,
        sequences: &[Vec<String>],
    ) -> GraphStoreResult<Self> {
        let mut paths = BTreeMap::new();
        for sequence in sequences {
            let mut pairs = Pairs::new();
            visit_pairs(store, graph, sequence, |pair| {
                pairs.insert(pair);
                Ok(())
            })?;
            paths.insert(sequence.clone(), pairs);
        }
        Ok(Self {
            storage: PathStorage::Memory(Arc::new(paths)),
        })
    }

    /// Bind a durable definition without reading any entity or reachability row.
    pub fn open_persistent(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        key: &str,
        graph: &str,
        sequences: &[Vec<String>],
    ) -> GraphStoreResult<Self> {
        Ok(Self {
            storage: PathStorage::Persistent(Arc::new(DurablePathIndex {
                catalog,
                backend,
                key: key.to_owned(),
                graph: graph.to_owned(),
                sequences: sequences.to_vec(),
                definition: serde_json::to_string(sequences).map_err(|error| json_error(&error))?,
                read_gate: parking_lot::ReentrantMutex::new(()),
            })),
        })
    }

    /// Build in bounded batches in one storage checkpoint. A failure preserves
    /// both the old definition and its materialization.
    pub fn build_persistent(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        key: &str,
        graph: &str,
        sequences: &[Vec<String>],
    ) -> GraphStoreResult<Self> {
        let definition = serde_json::to_string(sequences).map_err(|error| json_error(&error))?;
        let mut store =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        store.transaction(|store| {
            catalog.save_path_index(key, &definition)?;
            catalog.clear_path_index_data(key)?;
            for sequence in sequences {
                let sequence_key =
                    serde_json::to_string(sequence).map_err(|error| json_error(&error))?;
                let mut page = Vec::with_capacity(256);
                visit_pairs(store, graph, sequence, |pair| {
                    page.push(pair);
                    if page.len() == 256 {
                        catalog.save_path_index_pairs(key, &sequence_key, &page)?;
                        page.clear();
                    }
                    Ok(())
                })?;
                if !page.is_empty() {
                    catalog.save_path_index_pairs(key, &sequence_key, &page)?;
                }
            }
            catalog.finish_path_index_data(key, graph, &definition)?;
            Ok(())
        })?;
        Self::open_persistent(catalog, backend, key, graph, sequences)
    }

    /// Rebind only the physical session, never copy the index's data.
    pub fn rebind_persistent(
        &self,
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> GraphStoreResult<Self> {
        match &self.storage {
            PathStorage::Memory(_) | PathStorage::ReadView { .. } => Ok(self.clone()),
            PathStorage::Persistent(index) => {
                Self::open_persistent(catalog, backend, &index.key, &index.graph, &index.sequences)
            }
        }
    }

    /// Bind a transaction/cursor view whose own-write overlay cannot use
    /// reachability pages from the live storage transaction. Only requested
    /// sequences are evaluated; no resident index or graph is constructed.
    #[must_use]
    pub fn with_graph_read_view(&self, store: Arc<crate::GraphStoreHandle>) -> Self {
        let (graph, sequences) = match &self.storage {
            PathStorage::Memory(_) => return self.clone(),
            PathStorage::Persistent(index) => (&index.graph, &index.sequences),
            PathStorage::ReadView {
                graph, sequences, ..
            } => (graph, sequences),
        };
        Self {
            storage: PathStorage::ReadView {
                store,
                graph: graph.clone(),
                sequences: sequences.clone(),
            },
        }
    }

    pub fn lookup(&self, sequence: &[String]) -> GraphStoreResult<Option<Pairs>> {
        match &self.storage {
            PathStorage::Memory(paths) => Ok(paths.get(sequence).cloned()),
            PathStorage::ReadView {
                store,
                graph,
                sequences,
            } => {
                if !sequences.iter().any(|candidate| candidate == sequence) {
                    return Ok(None);
                }
                let mut pairs = Pairs::new();
                visit_pairs(store.as_ref(), graph, sequence, |pair| {
                    pairs.insert(pair);
                    Ok(())
                })?;
                Ok(Some(pairs))
            }
            PathStorage::Persistent(index) => {
                if !index
                    .sequences
                    .iter()
                    .any(|candidate| candidate == sequence)
                {
                    return Ok(None);
                }
                index.lookup(sequence).map(Some)
            }
        }
    }

    pub fn has_path(&self, sequence: &[String]) -> bool {
        match &self.storage {
            PathStorage::Memory(paths) => paths.contains_key(sequence),
            PathStorage::ReadView { sequences, .. } => {
                sequences.iter().any(|candidate| candidate == sequence)
            }
            PathStorage::Persistent(index) => index
                .sequences
                .iter()
                .any(|candidate| candidate == sequence),
        }
    }

    pub fn indexed_paths(&self) -> Vec<String> {
        let paths: BTreeSet<String> = match &self.storage {
            PathStorage::Memory(paths) => paths.keys().map(|sequence| sequence.join("/")).collect(),
            PathStorage::ReadView { sequences, .. } => sequences
                .iter()
                .map(|sequence| sequence.join("/"))
                .collect(),
            PathStorage::Persistent(index) => index
                .sequences
                .iter()
                .map(|sequence| sequence.join("/"))
                .collect(),
        };
        paths.into_iter().collect()
    }
}

impl DurablePathIndex {
    fn lookup(&self, sequence: &[String]) -> GraphStoreResult<Pairs> {
        struct ReadCheckpoint(Option<Arc<dyn PersistentStorageBackend>>);
        impl Drop for ReadCheckpoint {
            fn drop(&mut self) {
                if let Some(backend) = &self.0 {
                    let _ = backend.rollback_transaction();
                }
            }
        }
        let _read = self.read_gate.lock();
        if self.backend.in_transaction() {
            return self.lookup_in_snapshot(
                Arc::clone(&self.catalog),
                Arc::clone(&self.backend),
                sequence,
            );
        }
        // An escaped index handle must not borrow transaction ownership from
        // another concurrent direct query on its original engine session.
        let session = if self.backend.supports_concurrent_pinned_read_and_write() {
            self.backend.open_session()?
        } else {
            // Single-session providers are supported too; their caller owns
            // session serialization, as with direct GraphStore queries.
            uqa_storage::PersistentStorageSession::new(
                Arc::clone(&self.catalog),
                Arc::clone(&self.backend),
            )
        };
        let catalog = session.catalog;
        let backend = session.backend;
        backend.begin_read_transaction()?;
        let mut checkpoint = ReadCheckpoint(Some(Arc::clone(&backend)));
        let result = self.lookup_in_snapshot(catalog, Arc::clone(&backend), sequence);
        backend.rollback_transaction()?;
        checkpoint.0 = None;
        result
    }

    fn lookup_in_snapshot(
        &self,
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        sequence: &[String],
    ) -> GraphStoreResult<Pairs> {
        let definition = catalog
            .load_path_indexes()?
            .into_iter()
            .find_map(|(key, json)| (key == self.key).then_some(json));
        if definition.as_deref() != Some(self.definition.as_str()) {
            return Err(GraphStoreError::InvalidQuery(format!(
                "path index {:?} was dropped or redefined",
                self.key
            )));
        }
        let mut result = Pairs::new();
        if catalog.path_index_data_is_current(&self.key, &self.definition)? {
            let sequence_key =
                serde_json::to_string(sequence).map_err(|error| json_error(&error))?;
            let mut after = None;
            loop {
                let pairs = catalog.path_index_pairs(&self.key, &sequence_key, after, 256)?;
                if pairs.is_empty() {
                    break;
                }
                after = pairs.last().copied();
                result.extend(pairs);
            }
        } else {
            // A legacy or invalidated index is not a valid access path.
            // Evaluate only this requested sequence in the current read
            // snapshot; do not rebuild/retain a whole index or write on read.
            let store = PersistentGraphStore::from_catalog(catalog, backend);
            visit_pairs(&store, &self.graph, sequence, |pair| {
                result.insert(pair);
                Ok(())
            })?;
        }
        Ok(result)
    }
}

fn visit_pairs<G: GraphStore>(
    store: &G,
    graph: &str,
    sequence: &[String],
    mut visit: impl FnMut((u64, u64)) -> GraphStoreResult<()>,
) -> GraphStoreResult<()> {
    let mut after = None;
    loop {
        let ids = store.vertex_id_page(graph, after, 256)?;
        if ids.is_empty() {
            break;
        }
        after = ids.last().copied();
        for start in ids {
            let mut frontier = BTreeSet::from([start]);
            for label in sequence {
                let mut next = BTreeSet::new();
                for vertex in frontier {
                    next.extend(store.neighbors(vertex, Some(label), Direction::Out, graph)?);
                }
                frontier = next;
                if frontier.is_empty() {
                    break;
                }
            }
            for end in frontier {
                visit((start, end))?;
            }
        }
    }
    Ok(())
}
