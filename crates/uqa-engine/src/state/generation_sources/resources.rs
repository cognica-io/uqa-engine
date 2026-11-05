//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction catalog roots retain the same immutable resource owners as ordinary undo.

use crate::state::{CatalogCell, DurableCatalogSnapshot, SnapshotTableSources};
use uqa_storage::{memory_capture::MemoryCaptureRegistry, StorageBackendResult};

macro_rules! resource_sources {
    ($($field:ident: $source:ty => $image:ty, $producer:expr;)+) => {
        pub(super) struct ResourceSources {
            $($field: CatalogCell<$source, $image>,)+
        }

        impl ResourceSources {
            pub(super) fn new(registry: &MemoryCaptureRegistry, durable: &DurableCatalogSnapshot) -> StorageBackendResult<Self> {
                Ok(Self {
                    $($field: CatalogCell::capture_resource(durable.$field.clone(), registry, $producer)?,)+
                })
            }

            pub(super) fn select(&self, tables: &mut SnapshotTableSources) -> StorageBackendResult<()> {
                $(tables.select(&self.$field.key()?)?;)+
                Ok(())
            }

            #[cfg(test)]
            pub(super) fn keys(&self, control: &uqa_storage::read_control::StorageReadControl) -> StorageBackendResult<uqa_core::memory::Budgeted<ResourceKeys>> {
                let keys = ResourceKeys { $($field: self.$field.key()?,)+ };
                let mut bytes = size_of::<ResourceKeys>();
                $(bytes = bytes.checked_add(keys.$field.retained_source_bytes()).ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;)+
                Ok(uqa_core::memory::Budgeted::new(keys, control.memory().reserve(bytes)?))
            }
        }

        #[cfg(test)]
        pub(crate) struct ResourceKeys {
            $(pub(crate) $field: uqa_storage::memory_capture::MemoryCaptureKey<uqa_storage::memory_capture::CapturedMemoryValue<$image>>,)+
        }
    }
}

resource_sources! {
    graphs: uqa_graph::GraphCatalog => uqa_graph::CapturedGraphCatalog,
        uqa_graph::GraphCatalog::capture;
    models: uqa_ml::model_catalog::ModelCatalog => uqa_ml::model_catalog::CapturedModelCatalog,
        uqa_ml::model_catalog::ModelCatalog::capture;
    path_indexes: uqa_graph::PathIndexCatalog => uqa_graph::CapturedPathIndexes,
        uqa_graph::PathIndexCatalog::capture;
    named_analyzers: uqa_storage::analyzer_catalog::AnalyzerCatalog => uqa_storage::analyzer_catalog::CapturedAnalyzers,
        uqa_storage::analyzer_catalog::AnalyzerCatalog::capture;
    scoring_params: uqa_storage::catalog::scoring_params::ScoringParameters => uqa_storage::catalog::scoring_params::CapturedScoringParameters, uqa_storage::catalog::scoring_params::ScoringParameters::capture;
}
