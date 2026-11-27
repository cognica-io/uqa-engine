//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse the immutable definitions selected by this Engine session.

use super::{CatalogEpochs, CatalogReadView, DurableCatalogState, QueryRuntime, SessionContext};
use std::sync::Arc;
use uqa_core::memory::{MemoryBudget, RetainedValue};
use uqa_execution::catalog::{GraphCatalogOverlay, WeakCatalogReadGeneration};
use uqa_sql::catalog::temporary_namespace::TemporaryNamespaceOids;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct DefinitionRevision {
    tables: u64,
    registries: u64,
    session: u64,
}

impl DefinitionRevision {
    pub(super) fn new(epochs: CatalogEpochs, session: u64) -> Self {
        Self {
            tables: epochs.table_catalog,
            registries: epochs.catalog_registry,
            session,
        }
    }
}

#[derive(Default)]
pub(crate) struct CatalogViewCache {
    ordinary: Option<(DefinitionRevision, CatalogReadView)>,
    charged: Option<(DefinitionRevision, WeakCatalogReadGeneration)>,
}

impl CatalogViewCache {
    pub(super) fn get(
        &self,
        revision: DefinitionRevision,
        budget: Option<&MemoryBudget>,
    ) -> Option<CatalogReadView> {
        if let Some(budget) = budget {
            let (cached, generation) = self.charged.as_ref()?;
            if *cached == revision {
                return generation.upgrade_for(budget);
            }
            return None;
        }
        self.ordinary
            .as_ref()
            .filter(|(cached, _)| *cached == revision)
            .map(|(_, view)| view.clone())
    }

    pub(super) fn store(
        &mut self,
        revision: DefinitionRevision,
        view: &CatalogReadView,
        budget: Option<&MemoryBudget>,
    ) -> Option<CatalogReadView> {
        if budget.is_some() {
            self.charged = Some((revision, view.downgrade_generation()));
            None
        } else {
            self.ordinary
                .replace((revision, view.clone()))
                .map(|(_, old)| old)
        }
    }
}

impl QueryRuntime {
    pub(crate) fn invalidate_catalog_views(&self) {
        self.regtype_output_cache.clear();
        let retired = std::mem::take(&mut *self.catalog_views.lock());
        drop(retired);
    }
}

/// Graph overlays and sequence positions can change within one catalog epoch. Their selected owners distinguish those changes without inspecting table definitions.
pub(super) struct SessionDefinitions {
    pub(super) overlay: Option<GraphCatalogOverlay>,
    temporary: Option<TemporaryNamespaceOids>,
    graphs: RetainedValue<uqa_graph::GraphCatalog>,
    graph_oids: RetainedValue<uqa_sql::catalog::graph_oids::GraphOidRegistry>,
    sequences: RetainedValue<uqa_execution::catalog::sequence::registry::SequenceCatalog>,
}

impl SessionDefinitions {
    pub(super) fn current(
        session: &SessionContext,
        durable: &DurableCatalogState,
        include_overlay: bool,
    ) -> Self {
        let (overlay, temporary) = {
            let state = session.state.read();
            let overlay = if include_overlay {
                state
                    .graph_overlay
                    .as_ref()
                    .map(|graph| GraphCatalogOverlay {
                        names: graph.names.clone(),
                        store: graph.store.clone(),
                    })
            } else {
                None
            };
            (overlay, state.temporary_namespace)
        };
        Self {
            overlay,
            temporary,
            graphs: durable.graphs.snapshot(),
            graph_oids: durable.graph_catalog_oids.snapshot(),
            sequences: durable.sequences.snapshot(),
        }
    }

    pub(super) fn matches(&self, view: &CatalogReadView) -> bool {
        let selected = view.snapshot();
        let same_graphs = match (&self.overlay, &selected.graph_overlay) {
            (Some(current), Some(captured)) => {
                Arc::ptr_eq(&current.store, &captured.store)
                    && Arc::ptr_eq(&current.names, &captured.names)
            }
            (None, None) => true,
            _ => false,
        };
        same_graphs
            && self.temporary
                == selected
                    .temporary_namespace
                    .as_ref()
                    .map(|namespace| namespace.oids)
            && RetainedValue::ptr_eq(&self.graphs, &selected.definitions.graphs)
            && RetainedValue::ptr_eq(&self.graph_oids, &selected.definitions.graph_catalog_oids)
            && RetainedValue::ptr_eq(&self.sequences, &selected.definitions.sequences)
    }
}
