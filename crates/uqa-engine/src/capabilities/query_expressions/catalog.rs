//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain runtime catalog inputs until the session invalidates their generation.

use super::ScopedEngineHook;
use crate::Engine;
use parking_lot::Mutex;
use uqa_execution::catalog::{
    context::CatalogContext, services::CatalogSnapshotSource, CatalogReadView,
};
use uqa_execution::query::scalar_functions::ScalarFunctionContext;
use uqa_sql::SQLError;

pub(super) struct ScopedCatalog {
    initial_revision: Option<u64>,
    current: Mutex<Option<(u64, CatalogReadView)>>,
}

impl ScopedCatalog {
    pub(super) fn new(engine: &Engine, scope: &super::CteScope) -> Self {
        Self {
            initial_revision: scope
                .catalog_revision()
                .filter(|(source, _)| *source == engine.session_id)
                .map(|(_, revision)| revision),
            current: Mutex::new(None),
        }
    }
}

impl ScopedEngineHook<'_> {
    pub(super) fn catalog_context(&self) -> CatalogContext<'_> {
        CatalogContext {
            catalog: self,
            ..self.engine.catalog_execution()
        }
    }

    pub(super) fn scalar_function_context(&self) -> ScalarFunctionContext<'_> {
        ScalarFunctionContext {
            catalog: self.catalog_context(),
            ..self.engine.scalar_function_context()
        }
    }
}

impl CatalogSnapshotSource for ScopedEngineHook<'_> {
    fn catalog_snapshot(&self) -> CatalogReadView {
        let revision = self.engine.runtime.regtype_output_cache.revision();
        let mut current = self.catalog.current.lock();
        if let Some((selected, catalog)) = current.as_ref() {
            if *selected == revision {
                return catalog.clone();
            }
        }
        // Bound type names still use the original CTE catalog. Runtime inquiries follow private DDL and rollback; a portal worker's Engine itself retains its older catalog.
        let catalog = if Some(revision) == self.catalog.initial_revision {
            self.ctes
                .catalog_read_view()
                .unwrap_or_else(|_| self.engine.catalog_read_view())
        } else {
            self.engine.catalog_read_view()
        };
        *current = Some((revision, catalog.clone()));
        catalog
    }

    fn refreshed_catalog_snapshot(&self) -> Result<CatalogReadView, SQLError> {
        self.engine
            .synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load table catalog: {error}")))?;
        self.engine
            .synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load relation catalog: {error}")))?;
        Ok(self.catalog_snapshot())
    }

    fn current_catalog_snapshot(&self) -> CatalogReadView {
        self.engine.restored_catalog_read_view()
    }
}

impl uqa_sql::expr::CatalogInputFunctions for ScopedEngineHook<'_> {
    fn read_unknown_input(
        &self,
        text: &str,
        target: &uqa_sql::ColumnType,
    ) -> Result<uqa_core::Value, SQLError> {
        uqa_sql::expr::read_catalog_input(text, target, self)
    }
}

impl uqa_sql::expr::composites::CompositeTypeCatalog for ScopedEngineHook<'_> {
    fn composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<std::sync::Arc<uqa_sql::expr::composites::CompositeTypeDescriptor>>, SQLError>
    {
        let catalog = self.catalog_snapshot();
        if let Some(descriptor) = self
            .engine
            .runtime
            .composite_descriptor_cache
            .descriptor(&catalog.snapshot().definitions.composites, oid)
        {
            return Ok(Some(descriptor));
        }
        uqa_execution::catalog::composite_type::relations::descriptor(&self.catalog_context(), oid)
    }
}
