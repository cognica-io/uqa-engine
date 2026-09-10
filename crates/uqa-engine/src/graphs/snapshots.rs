//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical graph snapshot ownership and transaction-local write identities.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use uqa_graph::{GraphStore, GraphStoreHandle, PersistentGraphStore};
use uqa_storage::{CatalogFacade, PersistentStorageBackend, StorageBackendResult};

use crate::{Engine, GraphTransactionOverlay, SQLError};

type GraphHandles = BTreeMap<String, Arc<GraphStoreHandle>>;

fn snapshot_error(error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("capture physical graph snapshot: {error}"))
}

impl Engine {
    pub(super) fn with_implicit_graph_transaction<R>(
        &self,
        operation: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(super::graph_store_error)?;
            self.prepare_explicit_statement_snapshot(true)
                .map_err(super::graph_store_error)?;
        }
        self.with_implicit_storage_transaction(operation)
    }

    /// Only graph names and storage handles are collected here, not entities.
    pub(crate) fn visible_graph_handles(&self) -> Arc<GraphHandles> {
        if let Some(snapshot) = &self.query_catalog_snapshot {
            return Arc::clone(&snapshot.graphs);
        }
        if let Some(overlay) = &self.session.state.read().graph_overlay {
            let store = Arc::new(GraphStoreHandle::Persistent(overlay.store.as_ref().clone()));
            return Arc::new(
                overlay
                    .names
                    .iter()
                    .map(|name| (name.clone(), Arc::clone(&store)))
                    .collect(),
            );
        }
        self.durable.graphs.snapshot()
    }

    pub(crate) fn install_fixed_graph_snapshot(
        &self,
        snapshot: &PersistentGraphStore,
    ) -> Result<(), SQLError> {
        let GraphStoreHandle::Persistent(write) = self.new_graph_store().map_err(snapshot_error)?
        else {
            return Err(snapshot_error(
                "a fixed graph snapshot requires persistent storage",
            ));
        };
        let store = write.with_read_snapshot(snapshot);
        let names = Arc::new(
            store
                .graph_names()
                .map_err(snapshot_error)?
                .into_iter()
                .collect(),
        );
        self.session.state.write().graph_overlay = Some(GraphTransactionOverlay {
            store: Arc::new(store),
            names,
        });
        Ok(())
    }

    pub(super) fn graph_write_candidate(
        &self,
        name: &str,
        create: bool,
    ) -> StorageBackendResult<Option<GraphStoreHandle>> {
        if let Some(overlay) = &self.session.state.read().graph_overlay {
            return Ok((create || overlay.names.contains(name))
                .then(|| GraphStoreHandle::Persistent(overlay.store.fork_for_mutation())));
        }
        let existing = self.durable.graphs.read().get(name).cloned();
        match existing {
            Some(store) => Ok(Some(store.as_ref().clone())),
            None if create => self.new_graph_store().map(Some),
            None => Ok(None),
        }
    }

    /// Transaction overlays are session state, so catalog refresh must never
    /// publish one as the committed/live graph store of another session.
    pub(super) fn publish_graph_candidate(
        &self,
        candidate: GraphStoreHandle,
    ) -> StorageBackendResult<Arc<GraphStoreHandle>> {
        if self.session.state.read().graph_overlay.is_some() {
            let names = Arc::new(
                candidate
                    .graph_names()
                    .map_err(super::graph_store_error)?
                    .into_iter()
                    .collect(),
            );
            let live = Arc::new(self.new_graph_store()?);
            let GraphStoreHandle::Persistent(store) = candidate else {
                return Err(super::graph_store_error(
                    "persistent graph mutation produced a memory store",
                ));
            };
            self.session.state.write().graph_overlay = Some(GraphTransactionOverlay {
                store: Arc::new(store),
                names,
            });
            Ok(live)
        } else {
            Ok(Arc::new(candidate))
        }
    }

    /// Rollback-journal storage cannot retain a reader while promoting a
    /// writer. Preserve that exceptional fixed view on encrypted temporary
    /// storage, in bounded pages. Normal opens and new sessions never call
    /// this path and never hydrate graph entities.
    pub(crate) fn detach_graph_storage_snapshot(
        &self,
        graphs: &GraphHandles,
    ) -> Result<PersistentGraphStore, SQLError> {
        self.detach_selected_graph_storage_snapshot(graphs, None)
    }

    /// A cursor's graph view survives later writes and the owning SQL
    /// transaction. Prefer native pinned storage; spool only dependencies
    /// when this session's uncommitted writes prevent an independent reader.
    pub(crate) fn freeze_graph_read_handles(
        &self,
        required: Option<&BTreeSet<String>>,
        catalog_required: bool,
    ) -> Result<Arc<GraphHandles>, SQLError> {
        let graphs = self.visible_graph_handles();
        let Some(backend) = &self.storage.backend else {
            return Ok(graphs);
        };
        if self.query_catalog_snapshot.is_some()
            || (!catalog_required && required.is_some_and(BTreeSet::is_empty))
        {
            return Ok(graphs);
        }
        let overlay = self.session.state.read().graph_overlay.clone();
        let store = if let Some(snapshot) = overlay
            .as_ref()
            .and_then(|overlay| overlay.store.unmodified_read_snapshot())
        {
            snapshot
        } else if overlay.is_none()
            && backend.supports_concurrent_pinned_read_and_write()
            && !backend.transaction_has_written().map_err(snapshot_error)?
        {
            let snapshot: Arc<Engine> = self.open_independent_pinned_read_snapshot()?.into();
            let GraphStoreHandle::Persistent(store) =
                snapshot.new_graph_store().map_err(snapshot_error)?
            else {
                return Err(snapshot_error("persistent cursor has no graph storage"));
            };
            store.retain_resource(snapshot)
        } else {
            self.detach_selected_graph_storage_snapshot(&graphs, required)?
        };
        let handle = Arc::new(GraphStoreHandle::Persistent(store));
        Ok(Arc::new(
            graphs
                .keys()
                .map(|name| (name.clone(), Arc::clone(&handle)))
                .collect(),
        ))
    }

    fn detach_selected_graph_storage_snapshot(
        &self,
        graphs: &GraphHandles,
        required: Option<&BTreeSet<String>>,
    ) -> Result<PersistentGraphStore, SQLError> {
        let directory = tempfile::Builder::new()
            .prefix("uqa-graph-snapshot-")
            .tempdir()
            .map_err(snapshot_error)?;
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(snapshot_error)?;
        let mut encoded_key = String::with_capacity(64);
        for byte in key {
            write!(&mut encoded_key, "{byte:02x}").map_err(snapshot_error)?;
        }
        let connection = uqa_storage_sqlite::ManagedConnection::open_encrypted(
            &directory.path().join("snapshot.db"),
            &encoded_key,
        )
        .map_err(snapshot_error)?;
        let catalog: Arc<dyn CatalogFacade> = Arc::new(
            uqa_storage_sqlite::Catalog::open(connection.clone()).map_err(snapshot_error)?,
        );
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(uqa_storage_sqlite::SQLiteStorageBackend::new(connection));
        let mut snapshot = PersistentGraphStore::from_catalog(Arc::clone(&catalog), backend)
            .retain_resource(Arc::new(directory));
        snapshot
            .transaction(|target| {
                for (name, source) in graphs {
                    catalog.save_named_graph(name)?;
                    target.import_label_registry(name, &source.label_registry(name)?)?;
                    if required.is_some_and(|required| !required.contains(name)) {
                        continue;
                    }
                    let mut after = None;
                    loop {
                        self.runtime.cancellation.check().map_err(|error| {
                            uqa_graph::GraphStoreError::Storage(error.to_string())
                        })?;
                        let page = source.vertex_id_page(name, after, 256)?;
                        if page.is_empty() {
                            break;
                        }
                        after = page.last().copied();
                        for id in page {
                            let vertex = source.get_vertex(id)?.ok_or_else(|| {
                                uqa_graph::GraphStoreError::CorruptGraph(format!(
                                    "missing snapshot vertex {id}"
                                ))
                            })?;
                            catalog.save_vertex(
                                id,
                                &vertex.label,
                                &serde_json::to_string(&vertex.properties).map_err(|error| {
                                    uqa_graph::GraphStoreError::Storage(error.to_string())
                                })?,
                            )?;
                            catalog.save_graph_membership("vertex", id, name)?;
                        }
                    }
                    after = None;
                    loop {
                        self.runtime.cancellation.check().map_err(|error| {
                            uqa_graph::GraphStoreError::Storage(error.to_string())
                        })?;
                        let page = source.edge_id_page(name, after, 256)?;
                        if page.is_empty() {
                            break;
                        }
                        after = page.last().copied();
                        for id in page {
                            let edge = source.get_edge(id)?.ok_or_else(|| {
                                uqa_graph::GraphStoreError::CorruptGraph(format!(
                                    "missing snapshot edge {id}"
                                ))
                            })?;
                            // AGE intentionally retains incident edge rows after
                            // DROP LABEL. Copy the physical rows, including those
                            // tombstones, without reinterpreting them as inserts.
                            catalog.save_edge(
                                id,
                                edge.source_id,
                                edge.target_id,
                                &edge.label,
                                &serde_json::to_string(&edge.properties).map_err(|error| {
                                    uqa_graph::GraphStoreError::Storage(error.to_string())
                                })?,
                            )?;
                            catalog.save_graph_membership("edge", id, name)?;
                        }
                    }
                }
                Ok(())
            })
            .map_err(snapshot_error)?;
        Ok(snapshot)
    }
}
