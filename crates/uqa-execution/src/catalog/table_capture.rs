//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original relation membership selects its index metadata and resolved analyzer revisions.

use std::sync::Arc;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    analyzer_binding::registry::{CapturedFieldAnalyzers, FieldAnalyzerCatalog},
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendResult,
};

pub struct CapturedTableMetadata {
    pub indexes: Budgeted<crate::catalog::index::registry::CapturedCatalogIndexes>,
    pub analyzers: Budgeted<CapturedFieldAnalyzers>,
}

pub struct TableMetadataCaptureSource {
    indexes: Arc<crate::catalog::index::registry::CatalogIndexes>,
    analyzers: Arc<FieldAnalyzerCatalog>,
}

impl TableMetadataCaptureSource {
    /// Construct from the same retained generation as the table data and both metadata roots.
    pub fn new(
        indexes: Arc<crate::catalog::index::registry::CatalogIndexes>,
        analyzers: Arc<FieldAnalyzerCatalog>,
    ) -> Self {
        Self { indexes, analyzers }
    }

    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CapturedTableMetadata> {
        control.check()?;
        let indexes = self.indexes.capture(control)?;
        let analyzers = self.analyzers.capture(control)?;
        Ok(CapturedTableMetadata { indexes, analyzers })
    }
}

impl MemoryCaptureSource for TableMetadataCaptureSource {
    type Frozen = CapturedMemoryValue<CapturedTableMetadata>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let value = Budgeted::new(self.capture(control)?, control.memory().empty_reservation());
        CapturedMemoryValue::new(value, control)
    }
}

#[cfg(test)]
mod tests;
