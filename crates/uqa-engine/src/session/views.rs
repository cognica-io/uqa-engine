//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind public view lookups to the active catalog and session namespace.

use super::{Engine, SQLError, StoredView, StoredViewKind};
#[cfg(test)]
use super::{QueryPlan, StorageBackendError, StorageBackendResult};
#[cfg(test)]
use uqa_sql::binding::view_dependencies::restoration::bind_stored_view_relations;
#[cfg(test)]
use uqa_storage::RelationIdentity;

impl Engine {
    #[cfg(test)]
    pub(super) fn bind_stored_view_plan(
        &self,
        plan: &mut QueryPlan,
        relations: &std::collections::BTreeSet<RelationIdentity>,
    ) -> StorageBackendResult<()> {
        bind_stored_view_relations(plan, relations).map_err(StorageBackendError::Other)?;
        let mut refreshed = false;
        uqa_sql::binding::view_dependencies::bind_query_plan_sequence_references(
            plan,
            &mut |reference| {
                if !refreshed {
                    self.refresh_sequences_from_catalog()?;
                    refreshed = true;
                }
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)
            },
        )
    }

    pub(crate) fn stored_view_schema(
        &self,
        view: &StoredView,
    ) -> Result<uqa_execution::RowSchema, SQLError> {
        self.stored_view_schema_with_catalog(
            view,
            self.restored_catalog_read_view(),
            self.session_execution_view().relation_name_resolution(),
        )
    }

    pub(crate) fn stored_view_schema_with_catalog(
        &self,
        view: &StoredView,
        catalog: crate::capabilities::CatalogReadView,
        resolution: crate::capabilities::RelationNameResolution,
    ) -> Result<uqa_execution::RowSchema, SQLError> {
        view.row_schema(self, std::sync::Arc::new(catalog), resolution)
    }

    pub(crate) fn view_schema(
        &self,
        name: &str,
    ) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
        self.with_view_definition(name, |view| self.stored_view_schema(view))?
            .transpose()
    }

    pub(crate) fn view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        self.with_view_definition(name, Clone::clone)
    }

    /// Select the existing immutable catalog owner, then lend the definition without holding a registry lock through the reader.
    pub(crate) fn with_view_definition<R>(
        &self,
        name: &str,
        read: impl FnOnce(&StoredView) -> R,
    ) -> Result<Option<R>, SQLError> {
        let Some(resolved) = self
            .try_resolve_view_name(name)
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?
        else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&resolved)
            .map_err(|err| SQLError::Internal(format!("resolve view `{resolved}`: {err}")))?;
        let snapshot = self
            .query_view_snapshots
            .clone()
            .unwrap_or_else(|| self.durable.views.snapshot());
        Ok(snapshot.get(&relation).map(read))
    }

    /// Resolve a view only against the live restored registry without starting another registry synchronization pass.
    pub(crate) fn restored_catalog_view_definition(
        &self,
        name: &str,
    ) -> Result<Option<StoredView>, SQLError> {
        let views = self.durable.views.read();
        Ok(self
            .relation_lookup_candidates(name)
            .map_err(|error| {
                SQLError::Internal(format!("resolve restored view `{name}`: {error}"))
            })?
            .into_iter()
            .find_map(|relation| views.get(&relation).cloned()))
    }

    pub fn view(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        self.with_direct_read_snapshot(|engine| engine.view_plan(name))
    }

    pub(crate) fn view_plan(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        Ok(self
            .with_view_definition(name, |definition| {
                (definition.kind == StoredViewKind::View)
                    .then(|| definition.definition.query.clone())
            })?
            .flatten())
    }

    pub fn list_views(&self) -> Result<Vec<String>, SQLError> {
        self.with_direct_read_snapshot(|engine| {
            engine
                .synchronize_catalog_registries()
                .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
            let mut out: Vec<String> = engine
                .durable
                .views
                .read()
                .iter()
                .filter(|(_, view)| view.kind == StoredViewKind::View)
                .map(|(relation, _)| relation.qualified_name())
                .collect();
            out.sort_unstable();
            Ok(out)
        })
    }
}
