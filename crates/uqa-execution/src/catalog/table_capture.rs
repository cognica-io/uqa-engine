//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original relation membership selects its index metadata and resolved analyzer revisions.

use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    memory::{Budgeted, OwnedMap},
    RelationIdentity, RelationName,
};
use uqa_sql::ast::RelationPersistence;
use uqa_storage::{
    analyzer_binding::capture::{selected_fields, CapturedFieldAnalyzerBindings},
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    FieldAnalyzerBinding, StorageBackendError, StorageBackendResult,
};

pub struct CapturedTableMetadata {
    pub indexes: Budgeted<crate::catalog::index::registry::CapturedCatalogIndexes>,
    pub analyzers: Budgeted<CapturedFieldAnalyzerBindings>,
}

pub struct TableMetadataCaptureSource {
    tables: OwnedMap<RelationIdentity, RelationPersistence>,
    indexes: Arc<crate::catalog::index::registry::CatalogIndexes>,
    analyzers: Arc<BTreeMap<(String, String), FieldAnalyzerBinding>>,
}

impl TableMetadataCaptureSource {
    /// Construct from the same retained generation as the table data and both metadata roots.
    pub fn new(
        tables: impl IntoIterator<Item = (RelationIdentity, RelationPersistence)>,
        indexes: Arc<crate::catalog::index::registry::CatalogIndexes>,
        analyzers: Arc<BTreeMap<(String, String), FieldAnalyzerBinding>>,
    ) -> Self {
        let mut membership = OwnedMap::new();
        for (relation, persistence) in tables {
            membership.insert(relation, persistence);
        }
        Self {
            tables: membership,
            indexes,
            analyzers,
        }
    }

    fn selected(&self, name: &str) -> StorageBackendResult<bool> {
        let reference = RelationIdentity::borrowed_reference(name).ok_or_else(|| {
            StorageBackendError::Other(
                "captured table metadata has an invalid relation name".into(),
            )
        })?;
        let schema = reference.schema.unwrap_or(RelationName::decoded("public"));
        let persistence = self
            .tables
            .get_by(|relation| {
                schema
                    .cmp(&RelationName::decoded(&relation.schema))
                    .then_with(|| reference.name.cmp(&RelationName::decoded(&relation.name)))
            })
            .ok_or_else(|| {
                StorageBackendError::Other(
                    "captured table metadata has no original relation".into(),
                )
            })?;
        Ok(*persistence != RelationPersistence::Temporary)
    }

    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CapturedTableMetadata> {
        control.check()?;
        let indexes = self.indexes.capture(control)?;
        let analyzers = selected_fields(&self.analyzers, |name| self.selected(name), control)?;
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
