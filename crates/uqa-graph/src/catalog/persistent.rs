//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete persistent graph names precede ordered native label validation and private handle publication.

use super::GraphCatalog;
use crate::{memory_store::NativeLabels, GraphStoreError, PersistentGraphStore};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, OwnedMap, Produced, ProductionControl},
};
use uqa_storage::{
    catalog::input::{metadata, CatalogInput, CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    CatalogFacade, PersistentStorageBackend, StorageBackendError, StorageBackendResult,
};

pub struct GraphCatalogInput {
    input: ImageDecoder<GraphCatalog, StorageBackendError>,
}

impl GraphCatalogInput {
    /// Construct the graph catalog of a new private session. The supplied catalog and backend must share the caller's retained read transaction; this owner neither commits nor rolls it back. Complete physical membership is selected before label metadata is read.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        mutations: MemoryBudget,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let names = catalog.begin_graph_name_input(limits, control)?;
        let retained = control.clone();
        Ok(Self {
            input: ImageDecoder::new(
                control.memory(),
                control.cancellation(),
                |mut channel| async move {
                    let input = &mut channel.input();
                    let names = complete_names(names, &retained, input).await?;
                    let mut graphs = GraphCatalog::new(retained.memory());
                    for (name, ()) in names {
                        input.checkpoint().await?;
                        retained.check()?;
                        let graph = PersistentGraphStore::catalog_image_handle(
                            Arc::clone(&catalog),
                            Arc::clone(&backend),
                            &retained,
                        )?;
                        validate_labels(
                            &name,
                            catalog.as_ref(),
                            backend.as_ref(),
                            limits,
                            &retained,
                            input,
                        )
                        .await?;
                        let bytes = name.capacity();
                        graphs.graphs.insert(name, graph, bytes)?;
                        graphs.non_memory += 1;
                    }
                    input.checkpoint().await?;
                    retained.check()?;
                    Ok(graphs.into_mutation_budget(&mutations))
                },
            )?,
        })
    }

    /// Each poll bounds native row slots, JSON scanning and label/identity work. Every failure or abandonment consumes all unpublished names, handles and registry roots.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<GraphCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn complete_names(
    mut source: CatalogInput<String>,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<OwnedMap<Produced<String>, ()>> {
    let mut names = OwnedMap::new();
    loop {
        input.checkpoint().await?;
        control.check()?;
        match source.advance()? {
            CatalogInputStep::Pending => (),
            CatalogInputStep::Row(name) => {
                names.insert_budgeted(name.into(), (), control.memory())?;
            }
            CatalogInputStep::Complete => return Ok(names),
        }
    }
}

async fn validate_labels(
    name: &str,
    catalog: &dyn CatalogFacade,
    backend: &dyn PersistentStorageBackend,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    input.checkpoint().await?;
    control.check()?;
    if !catalog.named_graph_exists_with_control(name, limits, control)? {
        return Err(graph_error(GraphStoreError::UnknownGraph(name.to_owned())));
    }
    let text = metadata::read_from(
        catalog,
        "graph_label_registry::",
        name,
        limits,
        control,
        input,
    )
    .await?;
    let mut registry =
        NativeLabels::read_stored_from(text.as_deref().map(String::as_str), control, input).await?;
    drop(text);
    if let Some(allocator) = backend.identifier_allocator() {
        let text = metadata::read_from(
            catalog,
            "",
            "graph_identifier_generation",
            limits,
            control,
            input,
        )
        .await?;
        let generation = NativeLabels::read_allocation_identity_from(
            text.as_deref().map(String::as_str),
            control,
            input,
        )
        .await?;
        drop(text);
        let identifiers =
            crate::persistent_store::storage::GraphIdentifierScope::catalog(allocator, generation);
        let production =
            ProductionControl::new(control.memory(), control.cancellation(), input.cancellation);
        identifiers
            .restore_native_registry_from(name, &mut registry, &production, input)
            .await
            .map_err(graph_error)?;
    }
    Ok(())
}

fn graph_error(error: GraphStoreError) -> StorageBackendError {
    StorageBackendError::backend("graph catalog", error)
}
