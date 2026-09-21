//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained participant attribution belongs to semantic graph access, not physical decoding or validation reads.

use uqa_core::CancellationToken;
use uqa_storage::catalog::graph_observations::{
    scope_lifetime, GraphEntityKey, GraphMembershipKey, GraphSelectionKey,
};
use uqa_storage::mvcc::{SerializablePredicate, SerializableReadContext, VersionError};
use uqa_storage::{read_control::StorageReadControl, GraphEntityFilter, GraphEntityKind};

use super::PersistentGraphStore;
use crate::{GraphStoreError, GraphStoreResult};

#[derive(Clone)]
pub(super) struct GraphRead {
    context: SerializableReadContext,
    control: StorageReadControl,
}

impl GraphRead {
    fn observe(
        &self,
        namespace: uqa_storage::catalog::graph_identifiers::GraphIdentifierNamespace,
        predicate: SerializablePredicate<'_>,
    ) -> GraphStoreResult<()> {
        self.context
            .observe_read(scope_lifetime(namespace), &self.control)
            .map_err(VersionError::into_storage_error)?;
        self.context
            .observe_read(predicate, &self.control)
            .map_err(VersionError::into_storage_error)?;
        Ok(())
    }
}

impl PersistentGraphStore {
    /// A stored reachability result depends on its starting vertices and each selected edge label, including empty results. Register those logical selectors without reconstructing paths or observing entity properties.
    pub(crate) fn observe_cached_paths(
        &self,
        graph: &str,
        sequence: &[String],
    ) -> GraphStoreResult<()> {
        self.observe_selection(
            GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)),
            None,
        )?;
        for label in sequence {
            let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
            filter.label = Some(label);
            self.observe_selection(filter, None)?;
        }
        Ok(())
    }

    pub(super) fn observe_selection(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
    ) -> GraphStoreResult<()> {
        let Some(read) = &self.read else {
            return Ok(());
        };
        read.control.check()?;
        let mut previous = None;
        self.storage.visit_selection_namespaces(&mut |namespace| {
            let object = namespace.serializable_entity_object();
            if previous == Some(object) {
                return Ok(());
            }
            previous = Some(object);
            let key = GraphSelectionKey::new(namespace, filter, after)?;
            read.observe(namespace, key.predicate())?;
            Ok(())
        })
    }

    pub(super) fn observe_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: Option<&str>,
    ) -> GraphStoreResult<()> {
        let Some(read) = &self.read else {
            return Ok(());
        };
        read.control.check()?;
        let namespace = self
            .storage
            .entity_observation_namespace(kind, id)?
            .ok_or_else(|| {
                GraphStoreError::Storage(
                    "serializable graph memberships require an immutable namespace".into(),
                )
            })?;
        read.observe(
            namespace,
            GraphMembershipKey::new(namespace, kind, id, graph).predicate(),
        )?;
        Ok(())
    }

    pub(super) fn for_each_selected_id(
        &self,
        filter: GraphEntityFilter<'_>,
        visit: impl FnMut(u64) -> GraphStoreResult<()>,
    ) -> GraphStoreResult<()> {
        self.observe_selection(filter, None)?;
        self.for_each_id(filter, visit)
    }
    /// Bind a query's original participant and allowance without observing data. Clones retain this binding; catalog caches should keep unbound handles and bind each executing reader explicitly.
    pub fn with_serializable_read(
        &self,
        context: SerializableReadContext,
        cancellation: &CancellationToken,
    ) -> Self {
        let control = context.read_control(cancellation);
        let mut store = self.clone();
        store.read = Some(GraphRead { context, control });
        store
    }

    /// Remove a completed query's observer before retaining this handle as session catalog state. Its storage snapshot and resources are unchanged.
    pub fn without_serializable_read(mut self) -> Self {
        self.read = None;
        self
    }

    pub(super) fn observe_entity(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        if let Some(read) = &self.read {
            read.control.check()?;
            let namespace = self
                .storage
                .entity_observation_namespace(kind, id)?
                .ok_or_else(|| {
                    GraphStoreError::Storage(
                        "serializable graph reads require an immutable entity namespace".into(),
                    )
                })?;
            read.observe(
                namespace,
                GraphEntityKey::new(namespace, kind, id).predicate(),
            )?;
        }
        Ok(())
    }
}
