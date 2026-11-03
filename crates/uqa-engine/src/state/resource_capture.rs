//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry adapters retain immutable resource owners through their owning image interfaces.

use super::{CatalogCell, DurableCatalogState};
use std::collections::BTreeMap;
use uqa_core::memory::{Budgeted, BudgetedMap, ProductionControl};
use uqa_storage::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendResult,
};

pub(crate) type CapturedResources<T> = BudgetedMap<String, T>;

impl DurableCatalogState {
    pub(crate) fn memory(registry: &MemoryCaptureRegistry) -> StorageBackendResult<Self> {
        let mut state = Self::new();
        state.register_catalog_sources(registry)?;
        state.graphs =
            CatalogCell::capture_resource(state.graphs.snapshot(), registry, |values, control| {
                resources(
                    values,
                    control,
                    uqa_graph::GraphStoreHandle::capture_image_with_control,
                )
            })?;
        state.models =
            CatalogCell::capture_resource(state.models.snapshot(), registry, |values, control| {
                resources(
                    values,
                    control,
                    uqa_ml::DeepModel::capture_image_with_control,
                )
            })?;
        state.path_indexes = CatalogCell::capture_resource(
            state.path_indexes.snapshot(),
            registry,
            |values, control| {
                resources(
                    values,
                    control,
                    uqa_graph::PathIndex::capture_memory_with_control,
                )
            },
        )?;
        state.named_analyzers = CatalogCell::capture_resource(
            state.named_analyzers.snapshot(),
            registry,
            |values, control| {
                resources(values, control, |source, control| {
                    uqa_storage::analyzer_image::AnalyzerImageSnapshot::capture(source, control)
                })
            },
        )?;
        state.table_field_analyzers = CatalogCell::capture_resource(
            state.table_field_analyzers.snapshot(),
            registry,
            uqa_storage::analyzer_binding::capture::fields,
        )?;
        Ok(state)
    }
}

fn resources<T, F>(
    values: &BTreeMap<String, T>,
    control: &StorageReadControl,
    capture: fn(&T, &StorageReadControl) -> StorageBackendResult<F>,
) -> StorageBackendResult<Budgeted<CapturedResources<F>>> {
    control.check()?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let mut output = BudgetedMap::new(control.memory());
    let mut memory = control.memory().empty_reservation();
    for (name, source) in values {
        control.check()?;
        let (name, charge) = production.copy_text(name)?.into_parts();
        memory.absorb(charge.expect("resource names retain their capture allowance"));
        let resource = capture(source, control)?;
        output.insert(name, resource)?;
    }
    control.check()?;
    Ok(Budgeted::new(output, memory))
}
