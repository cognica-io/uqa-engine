//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind retained namespace readers to the invoking planner's production control.

use crate::Engine;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_execution::catalog::services::CatalogSession;
use uqa_sql::{
    catalog::resolution::{RelationLookupMode, RelationNameResolution, RelationResolution},
    SQLError,
};

impl Engine {
    pub(crate) fn try_resolve_view_name_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        self.with_view_definition_with_control(name, control, |identity, _| {
            Ok(identity.qualified_name_with_control(control)?)
        })
    }

    pub(crate) fn with_view_definition_with_control<R>(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: impl FnOnce(&uqa_core::RelationIdentity, &crate::StoredView) -> Result<R, SQLError>,
    ) -> Result<Option<R>, SQLError> {
        control.check()?;
        if self.query_view_snapshots.is_none() {
            self.synchronize_catalog_registries()
                .map_err(|error| SQLError::Internal(format!("refresh view catalog: {error}")))?;
        }
        let candidates =
            uqa_sql::catalog::resolution::candidates::relation_lookup_candidates_with_control(
                self, name, control,
            )?;
        let views = self
            .query_view_snapshots
            .clone()
            .unwrap_or_else(|| self.durable.views.snapshot());
        for relation in candidates.iter() {
            control.check()?;
            if let Some(view) = views.get(relation) {
                return read(relation, view).map(Some);
            }
        }
        Ok(None)
    }

    pub(crate) fn mutation_view_kind_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<crate::StoredViewKind>, SQLError> {
        let candidates =
            uqa_sql::catalog::resolution::candidates::relation_lookup_candidates_with_control(
                self, name, control,
            )?;
        let tables = self.storage.tables.read();
        let views = self.durable.views.read();
        for relation in candidates.iter() {
            control.check()?;
            if tables.contains_key(relation) {
                return Ok(None);
            }
            if let Some(view) = views.get(relation) {
                return Ok(Some(view.kind));
            }
        }
        Ok(None)
    }
    pub(crate) fn resolve_relation_kind_for_query_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        control.check()?;
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load table catalog: {error}")))?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load relation catalog: {error}")))?;
        self.resolve_loaded_relation_kind_with_control(name, bound, control)
    }

    pub(crate) fn resolve_loaded_relation_kind_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        let resolution = self
            .session_execution_view()
            .relation_name_resolution_with_control(control)?;
        let (mut resolution, memory) = resolution.into_parts();
        if bound {
            resolution.set_lookup_mode(RelationLookupMode::Bound);
        }
        let resolution = control.finish(resolution, memory)?;
        self.relation_kind_from_catalog_with_control(&resolution, name, control)
    }

    fn relation_kind_from_catalog_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        if let Some(snapshot) = &self.query_catalog_snapshot {
            return snapshot
                .read_view
                .relation_kind_resolution_with_control(resolution, name, control);
        }
        let durable = self.durable.snapshot();
        let graph_names = self
            .session
            .state
            .read()
            .graph_overlay
            .as_ref()
            .map(|overlay| overlay.names.clone());
        let definitions = Self::catalog_definition_snapshot(&durable);
        let tables = self.storage.tables.read();
        let names = uqa_execution::catalog::relation_names::RelationNameCatalog::new(
            &*tables,
            &definitions,
        );
        let names = match &graph_names {
            Some(graphs) => names.with_graph_names(graphs),
            None => names,
        };
        names.relation_kind_resolution_with_control(resolution, name, control)
    }

    pub(crate) fn resolve_mutation_target_name_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        let resolution = if bound {
            self.resolve_loaded_relation_kind_with_control(name, true, control)?
        } else {
            self.resolve_relation_kind_for_query_with_control(name, false, control)?
        };
        let (resolution, memory) = resolution.into_parts();
        match resolution {
            RelationResolution::Found(name, _) => Ok(control.finish(name, memory)?),
            _ => Err(SQLError::UnknownTable(
                control.copy_text(name)?.into_parts().0,
            )),
        }
    }
}

impl Engine {
    /// Retain a table's immutable declaration generation after the original catalog and data refreshes, releasing registry and column guards before callbacks.
    pub(crate) fn table_column_snapshot_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<std::sync::Arc<Vec<uqa_sql::ast::ColumnDef>>>, SQLError> {
        control.check()?;
        self.synchronize_table_catalog()
            .and_then(|()| self.synchronize_table_data())
            .map_err(|error| {
                match uqa_execution::storage_errors::storage_error(
                    "read table declarations",
                    &error,
                ) {
                    SQLError::Internal(_) => match control.format(format_args!("{error}")) {
                        Ok(message) => SQLError::Internal(message.into_parts().0),
                        Err(error) => error.into(),
                    },
                    diagnostic => diagnostic,
                }
            })?;
        let candidates =
            uqa_sql::catalog::resolution::candidates::relation_lookup_candidates_with_control(
                self, name, control,
            )?;
        let table = {
            let tables = self.storage.tables.read();
            candidates
                .iter()
                .find_map(|relation| tables.get(relation).cloned())
        };
        control.check()?;
        Ok(table.map(|table| table.columns.snapshot()))
    }
}
