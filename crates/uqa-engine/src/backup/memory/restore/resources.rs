//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private destination adopts resource definitions through each resource's native owner.

use crate::state::{CatalogCell, CatalogGeneration, DurableCatalogState};
use std::io::Read;
use uqa_analysis::AnalyzerResources;
use uqa_core::memory::{Budgeted, MemoryBudget, ProductionControl};
use uqa_graph::{GraphCatalog, PathIndexCatalog};
use uqa_ml::model_catalog::ModelCatalog;
use uqa_storage::{
    analyzer_binding::{registry::FieldAnalyzerCatalog, FieldAnalyzerBinding},
    analyzer_catalog::AnalyzerCatalog,
    backup::DecodedMemoryImageAddress,
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(super) struct ResourcesInput {
    pub fields: FieldAnalyzerCatalog,
    graphs: GraphCatalog,
    models: ModelCatalog,
    paths: PathIndexCatalog,
    analyzers: AnalyzerCatalog,
}

impl ResourcesInput {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            fields: FieldAnalyzerCatalog::new(memory),
            graphs: GraphCatalog::new(memory),
            models: ModelCatalog::new(memory),
            paths: PathIndexCatalog::new(memory),
            analyzers: AnalyzerCatalog::new(memory),
        }
    }

    pub(super) fn read(
        mut self,
        address: &DecodedMemoryImageAddress,
        reader: &mut dyn Read,
        resources: &AnalyzerResources,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        match address {
            DecodedMemoryImageAddress::FieldAnalyzer { table, field } => {
                let (table, table_memory) = production.copy_text(table)?.into_parts();
                let (field, field_memory) = production.copy_text(field)?.into_parts();
                let key = production
                    .finish(
                        (table, field),
                        production.combine(table_memory, field_memory),
                    )?
                    .into_budgeted()
                    .expect("native field identity admission");
                let binding = FieldAnalyzerBinding::read_image(reader, resources, control)?;
                self.fields = self.fields.insert_image(key, binding, control)?;
            }
            DecodedMemoryImageAddress::Resource { kind, name } => {
                let name: Budgeted<String> = production
                    .copy_text(name)?
                    .into_budgeted()
                    .expect("native resource identity admission");
                match kind {
                    1 => self.graphs = self.graphs.read_image(name, reader, mutations, control)?,
                    2 => {
                        let value = uqa_ml::model_image::read_model_image(reader, control)?;
                        self.models = self.models.insert_image(name, value, control)?;
                    }
                    3 => self.paths = self.paths.read_image(name, reader, control)?,
                    4 => {
                        let value = uqa_storage::analyzer_image::read_analyzer_image(
                            reader, resources, control,
                        )?;
                        self.analyzers = self.analyzers.insert_image(name, value, control)?;
                    }
                    _ => return Err(invalid()),
                }
            }
            _ => return Err(invalid()),
        }
        control.check()?;
        Ok(self)
    }

    pub(super) fn finish(
        self,
        state: &mut DurableCatalogState,
        registry: &MemoryCaptureRegistry,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        macro_rules! install {
            ($field:ident, $value:expr, $capture:expr) => {
                state.$field = CatalogCell::capture_resource_with_control(
                    CatalogGeneration::with_budget(
                        $value.into_mutation_budget(mutations),
                        control.memory(),
                    )?,
                    registry,
                    $capture,
                    control,
                )?;
            };
        }
        install!(
            table_field_analyzers,
            self.fields,
            FieldAnalyzerCatalog::capture
        );
        install!(graphs, self.graphs, GraphCatalog::capture);
        install!(models, self.models, ModelCatalog::capture);
        install!(path_indexes, self.paths, PathIndexCatalog::capture);
        install!(named_analyzers, self.analyzers, AnalyzerCatalog::capture);
        control.check()?;
        Ok(())
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("unexpected native resource image".into())
}
