//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt graph identity publication to retained Engine catalog roots and the existing graph transaction.

use crate::Engine;
use std::collections::BTreeSet;
use uqa_execution::catalog::graph_oids::changes::{self, GraphOidChange};
use uqa_graph::GraphStoreHandle;
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(super) fn record_created_graph_oids(
        &self,
        candidate: &GraphStoreHandle,
    ) -> StorageBackendResult<()> {
        let published = self.published_graph_names();
        let current = self.durable.graph_catalog_oids.read().clone();
        let change = changes::created(
            &current,
            &published,
            candidate,
            self.catalog_identity_reservation_context(),
            |oid| Ok(uqa_execution::schema::namespaces::identity::namespace_oid_in_use(self, oid)),
        )?;
        self.install_graph_oid_change(change)
    }

    pub(super) fn forget_graph_label_oids(
        &self,
        graph: &str,
        label_ids: &[u32],
    ) -> StorageBackendResult<()> {
        let current = self.durable.graph_catalog_oids.read().clone();
        self.install_graph_oid_change(changes::labels_removed(&current, graph, label_ids)?)
    }

    pub(super) fn forget_graph_oids(&self, graph: &str) -> StorageBackendResult<()> {
        let current = self.durable.graph_catalog_oids.read().clone();
        self.install_graph_oid_change(changes::removed(&current, graph)?)
    }

    pub(super) fn rename_graph_oids(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let current = self.durable.graph_catalog_oids.read().clone();
        self.install_graph_oid_change(changes::renamed(&current, from, to)?)
    }

    fn install_graph_oid_change(&self, change: Option<GraphOidChange>) -> StorageBackendResult<()> {
        if let Some(change) = change {
            let next = change.persist(self.storage.catalog.as_deref())?;
            *self.durable.graph_catalog_oids.write() = next;
        }
        Ok(())
    }

    /// The graphs visible before the write being published: the session's transaction overlay, or the published registry.
    fn published_graph_names(&self) -> BTreeSet<String> {
        if let Some(overlay) = &self.session.state.read().graph_overlay {
            return overlay.names.iter().cloned().collect();
        }
        self.durable.graphs.read().keys().cloned().collect()
    }
}
