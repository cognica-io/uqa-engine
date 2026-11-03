//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction catalog roots retain the same immutable resource owners as ordinary undo.

use crate::state::{
    resource_capture::{resources, CapturedResources},
    CatalogCell, DurableCatalogSnapshot, SnapshotTableSources,
};
use std::{collections::BTreeMap, sync::Arc};
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
    graphs: BTreeMap<String, Arc<uqa_graph::GraphStoreHandle>> => CapturedResources<uqa_graph::GraphImageSnapshot>,
        |values, control| resources(values, control, uqa_graph::GraphStoreHandle::capture_image_with_control);
    models: BTreeMap<String, Arc<uqa_ml::DeepModel>> => CapturedResources<uqa_ml::model_image::ModelImageSnapshot>,
        |values, control| resources(values, control, uqa_ml::DeepModel::capture_image_with_control);
    path_indexes: BTreeMap<String, uqa_graph::PathIndex> => CapturedResources<uqa_core::memory::Budgeted<uqa_graph::CapturedPathIndex>>,
        |values, control| resources(values, control, uqa_graph::PathIndex::capture_memory_with_control);
    named_analyzers: BTreeMap<String, Arc<uqa_analysis::CompiledAnalyzer>> => CapturedResources<uqa_storage::analyzer_image::AnalyzerImageSnapshot>,
        |values, control| resources(values, control, |source, control| uqa_storage::analyzer_image::AnalyzerImageSnapshot::capture(source, control));
    scoring_params: BTreeMap<String, String> => Vec<(String, String)>, uqa_storage::catalog::capture::scoring_params;
}
