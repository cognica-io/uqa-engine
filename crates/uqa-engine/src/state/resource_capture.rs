//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry adapters retain immutable resource owners through their owning image interfaces.

use super::{CatalogCell, DurableCatalogState};
use uqa_storage::{memory_capture::MemoryCaptureRegistry, StorageBackendResult};

impl DurableCatalogState {
    pub(crate) fn memory(registry: &MemoryCaptureRegistry) -> StorageBackendResult<Self> {
        let mut state = Self::new();
        state.register_catalog_sources(registry)?;
        state.graphs = CatalogCell::capture_resource(
            state.graphs.snapshot(),
            registry,
            uqa_graph::GraphCatalog::capture,
        )?;
        state.models = CatalogCell::capture_resource(
            state.models.snapshot(),
            registry,
            uqa_ml::model_catalog::ModelCatalog::capture,
        )?;
        state.path_indexes = CatalogCell::capture_resource(
            state.path_indexes.snapshot(),
            registry,
            uqa_graph::PathIndexCatalog::capture,
        )?;
        state.named_analyzers = CatalogCell::capture_resource(
            state.named_analyzers.snapshot(),
            registry,
            uqa_storage::analyzer_catalog::AnalyzerCatalog::capture,
        )?;
        state.table_field_analyzers = CatalogCell::capture_resource(
            state.table_field_analyzers.snapshot(),
            registry,
            uqa_storage::analyzer_binding::registry::FieldAnalyzerCatalog::capture,
        )?;
        Ok(state)
    }
}
