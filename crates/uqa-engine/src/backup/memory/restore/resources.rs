//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private destination adopts resource definitions through each resource's native owner.

use crate::state::{CatalogCell, CatalogGeneration, DurableCatalogState};
use std::io::Read;
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
};
use uqa_graph::{GraphCatalog, PathIndexCatalog};
use uqa_ml::model_catalog::ModelCatalog;
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog,
    analyzer_catalog::AnalyzerCatalog,
    backup::{DecodedMemoryImageAddress, MemoryImageName},
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(super) struct ResourcesInput {
    stage: Stage,
}

enum Stage {
    Ready(Budgeted<Box<Resources>>),
    Reading {
        selection: Selection,
        input: ImageDecoder<Budgeted<Box<Resources>>, StorageBackendError>,
    },
}

#[derive(Clone)]
enum Selection {
    Named {
        kind: u8,
        name: MemoryImageName,
    },
    Field {
        table: MemoryImageName,
        field: MemoryImageName,
    },
}

impl Selection {
    fn from_address(address: &DecodedMemoryImageAddress) -> StorageBackendResult<Self> {
        match address {
            DecodedMemoryImageAddress::Resource {
                kind: kind @ 1..=4,
                name,
            } => Ok(Self::Named {
                kind: *kind,
                name: name.clone(),
            }),
            DecodedMemoryImageAddress::FieldAnalyzer { table, field } => Ok(Self::Field {
                table: table.clone(),
                field: field.clone(),
            }),
            _ => Err(invalid()),
        }
    }

    fn matches(&self, address: &DecodedMemoryImageAddress) -> bool {
        match (self, address) {
            (
                Self::Named { kind, name },
                DecodedMemoryImageAddress::Resource {
                    kind: selected,
                    name: selected_name,
                },
            ) => kind == selected && std::ptr::eq(name.as_ref(), selected_name.as_ref()),
            (
                Self::Field { table, field },
                DecodedMemoryImageAddress::FieldAnalyzer {
                    table: selected_table,
                    field: selected_field,
                },
            ) => {
                std::ptr::eq(table.as_ref(), selected_table.as_ref())
                    && std::ptr::eq(field.as_ref(), selected_field.as_ref())
            }
            _ => false,
        }
    }
}

struct Resources {
    fields: FieldAnalyzerCatalog,
    graphs: GraphCatalog,
    models: ModelCatalog,
    paths: PathIndexCatalog,
    analyzers: AnalyzerCatalog,
}

impl ResourcesInput {
    pub(super) fn new(memory: &MemoryBudget) -> StorageBackendResult<Self> {
        let header = memory.reserve(size_of::<Resources>())?;
        Ok(Self {
            stage: Stage::Ready(Budgeted::new(Box::new(Resources::new(memory)), header)),
        })
    }

    pub(super) fn fields(&self) -> StorageBackendResult<&FieldAnalyzerCatalog> {
        match &self.stage {
            Stage::Ready(resources) => Ok(&resources.fields),
            Stage::Reading { .. } => Err(invalid()),
        }
    }

    pub(super) fn read(
        self,
        address: &DecodedMemoryImageAddress,
        reader: &mut dyn Read,
        resources: &AnalyzerResources,
        mutations: &MemoryBudget,
        window: ImageDecodeWindow,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Self, bool)> {
        control.check()?;
        if window.bytes == 0 || window.steps == 0 {
            return Err(invalid());
        }
        let (selection, mut input) = match self.stage {
            Stage::Ready(ready) => {
                let selection = Selection::from_address(address)?;
                let input = Resources::input(
                    ready,
                    selection.clone(),
                    resources.clone(),
                    mutations.clone(),
                    control,
                )?;
                (selection, input)
            }
            Stage::Reading { selection, input } => {
                if !selection.matches(address) {
                    return Err(invalid());
                }
                (selection, input)
            }
        };
        Ok(match input.advance(reader, window)? {
            Some(ready) => (
                Self {
                    stage: Stage::Ready(ready),
                },
                true,
            ),
            None => (
                Self {
                    stage: Stage::Reading { selection, input },
                },
                false,
            ),
        })
    }

    pub(super) fn finish(
        self,
        state: &mut DurableCatalogState,
        registry: &MemoryCaptureRegistry,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        match self.stage {
            Stage::Ready(resources) => resources
                .try_map(|resources| resources.finish(state, registry, mutations, control))
                .map(|_| ()),
            Stage::Reading { .. } => Err(invalid()),
        }
    }
}

impl Resources {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            fields: FieldAnalyzerCatalog::new(memory),
            graphs: GraphCatalog::new(memory),
            models: ModelCatalog::new(memory),
            paths: PathIndexCatalog::new(memory),
            analyzers: AnalyzerCatalog::new(memory),
        }
    }

    fn input(
        ready: Budgeted<Box<Self>>,
        selection: Selection,
        analyzers: AnalyzerResources,
        mutations: MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageDecoder<Budgeted<Box<Self>>, StorageBackendError>> {
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            move |mut channel| async move {
                ready
                    .try_map_async(move |mut resources| async move {
                        let mut input = channel.input();
                        match selection {
                            Selection::Named { kind, name } => {
                                let name = input.copy_text(&name).await?;
                                match kind {
                                    1 => {
                                        resources.graphs = resources
                                            .graphs
                                            .read_image_from(name, &mutations, &mut input)
                                            .await?;
                                    }
                                    2 => {
                                        resources.models = resources
                                            .models
                                            .read_image_from(name, &mut input)
                                            .await?;
                                    }
                                    3 => {
                                        resources.paths = resources
                                            .paths
                                            .read_image_from(name, &mut input)
                                            .await?;
                                    }
                                    4 => {
                                        resources.analyzers = resources
                                            .analyzers
                                            .read_image_from(name, &analyzers, &mut input)
                                            .await?;
                                    }
                                    _ => return Err(invalid()),
                                }
                            }
                            Selection::Field { table, field } => {
                                let table = input.copy_text(&table).await?;
                                let field = input.copy_text(&field).await?;
                                let (table, mut memory) = table.into_parts();
                                let (field, field_memory) = field.into_parts();
                                memory.absorb(field_memory);
                                let key = Budgeted::new((table, field), memory);
                                resources.fields = resources
                                    .fields
                                    .read_image_from(key, &analyzers, &mut input)
                                    .await?;
                            }
                        }
                        Ok(resources)
                    })
                    .await
            },
        )?)
    }

    fn finish(
        self: Box<Self>,
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
                    Some(control),
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

#[cfg(test)]
mod tests;
