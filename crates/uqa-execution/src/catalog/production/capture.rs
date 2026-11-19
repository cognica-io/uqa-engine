//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected catalog roots become a complete read view through the same table and header ownership boundary.

use crate::catalog::{
    CatalogDefinitionSnapshot, CatalogReadSnapshot, CatalogReadView, CatalogTableSnapshot,
    GraphCatalogOverlay,
};
use std::sync::Arc;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    catalog::temporary_namespace::{TemporaryNamespace, TemporaryNamespaceOids},
    SQLError,
};

/// Retained definition and session metadata for a catalog read. Source owners select the table generation separately and keep it alive throughout capture; query read attribution and live sequence positions remain at their existing adapter boundary.
pub struct CatalogReadCapture {
    definitions: CatalogDefinitionSnapshot,
    graph_overlay: Option<GraphCatalogOverlay>,
    temporary_namespace: Option<(u64, TemporaryNamespaceOids)>,
    prepared_catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
}

impl CatalogReadCapture {
    pub fn new(definitions: CatalogDefinitionSnapshot) -> Self {
        Self {
            definitions,
            graph_overlay: None,
            temporary_namespace: None,
            prepared_catalog: None,
        }
    }

    #[must_use]
    pub fn with_graph_overlay(mut self, overlay: Option<GraphCatalogOverlay>) -> Self {
        self.graph_overlay = overlay;
        self
    }

    #[must_use]
    pub fn with_temporary_namespace(
        mut self,
        session_id: u64,
        oids: Option<TemporaryNamespaceOids>,
    ) -> Self {
        self.temporary_namespace = oids.map(|oids| (session_id, oids));
        self
    }

    #[must_use]
    pub fn with_prepared_catalog(
        mut self,
        catalog: Option<Arc<dyn uqa_storage::CatalogFacade>>,
    ) -> Self {
        self.prepared_catalog = catalog;
        self
    }

    pub fn capture_with_control<'a>(
        self,
        tables: impl IntoIterator<Item = (&'a RelationIdentity, CatalogTableSnapshot)>,
        control: &ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        let tables = CatalogReadSnapshot::capture_tables_with_control(tables, control)?;
        self.finish(tables, control)
    }

    /// Table names, membership nodes and comparisons consume the invoking continuation. Completion admits only the fixed read-view/cache headers and, when present, the bounded session-number spelling of its temporary namespace.
    pub async fn capture_from_input<'a>(
        self,
        tables: impl IntoIterator<Item = (&'a RelationIdentity, CatalogTableSnapshot)>,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        let tables =
            CatalogReadSnapshot::capture_tables_from_input(tables, memory, original, input).await?;
        control.check()?;
        input.checkpoint().await?;
        self.finish(tables, &control)
    }

    fn finish(
        self,
        tables: OwnedMap<RelationIdentity, CatalogTableSnapshot>,
        control: &ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        control.check()?;
        let mut memory = control.empty_reservation();
        let temporary_namespace = if let Some((session_id, oids)) = self.temporary_namespace {
            let (schema, lease) = control
                .format(format_args!("pg_temp_{session_id}"))?
                .into_parts();
            memory = control.combine(memory, lease);
            Some(TemporaryNamespace { schema, oids })
        } else {
            None
        };
        let view = CatalogReadView::new_with_control(
            control.finish(
                CatalogReadSnapshot {
                    tables,
                    definitions: self.definitions,
                    graph_overlay: self.graph_overlay,
                    temporary_namespace,
                },
                memory,
            )?,
            control,
        )?;
        Ok(match self.prepared_catalog {
            Some(catalog) => view.with_prepared_catalog(catalog),
            None => view,
        })
    }
}

#[cfg(test)]
mod tests;
