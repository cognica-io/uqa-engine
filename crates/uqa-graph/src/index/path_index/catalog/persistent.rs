//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind logical path names to their original physical materializations in one catalog snapshot.

use super::{PathIndex, PathIndexCatalog};
use crate::{image_work::CatalogNames, GraphCatalog};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, Produced, RetainedValue},
};
use uqa_storage::{
    catalog::{input::CatalogInputLimits, path_index, text_input::CatalogTextFamily},
    read_control::StorageReadControl,
    CatalogFacade, PersistentStorageBackend, StorageBackendError, StorageBackendResult,
};

mod definition;

pub struct PathIndexCatalogInput {
    input: ImageDecoder<PathIndexCatalog, StorageBackendError>,
}

impl PathIndexCatalogInput {
    /// Bind definitions in their provider's ordinary order on the caller's private transaction. Materialized pairs are never read by this owner.
    pub fn new(
        graphs: GraphCatalog,
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        mutations: MemoryBudget,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                input.checkpoint().await?;
                let mut source =
                    catalog.begin_text_load(CatalogTextFamily::PathIndexes, limits, &retained)?;
                let rows = loop {
                    input.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop(source);
                let bindings = Bindings {
                    graphs: &graphs,
                    catalog: &catalog,
                    backend: &backend,
                    read: Some((limits, &retained)),
                };
                let mut paths = PathIndexCatalog::new(retained.memory());
                let (rows, _row_memory) = rows.into_parts();
                for row in rows {
                    input.checkpoint().await?;
                    let (row, mut memory) = row.into_budgeted().into_parts();
                    let name_memory = memory.split(row.name.capacity());
                    let name = Budgeted::new(row.name, name_memory);
                    let text = Budgeted::new(row.value, memory);
                    bindings
                        .bind(&mut paths, name, &text, None, &mut input)
                        .await?;
                }
                retained.check()?;
                Ok(paths.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    /// Return the complete usable registry once. Failure and abandonment release the source, partial bindings and every uninstalled registration.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<PathIndexCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

struct Bindings<'a> {
    graphs: &'a GraphCatalog,
    catalog: &'a Arc<dyn CatalogFacade>,
    backend: &'a Arc<dyn PersistentStorageBackend>,
    read: Option<(CatalogInputLimits, &'a StorageReadControl)>,
}

impl Bindings<'_> {
    async fn bind(
        &self,
        candidate: &mut PathIndexCatalog,
        physical: Budgeted<String>,
        text: &str,
        changed: Option<&BTreeSet<String>>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        // Cold opening diagnoses the original declaration before its graph. A selective refresh leaves other declarations and materializations untouched.
        let definition = if let Some(names) = changed {
            if !path_index::may_belong_to(&physical, |graph| names.contains(graph)) {
                return Ok(());
            }
            None
        } else {
            Some(definition::read(text, input).await?)
        };
        let recorded = if let Some((limits, control)) = self.read {
            self.catalog
                .path_index_graph_with_control(&physical, limits, control)?
        } else {
            self.catalog
                .path_index_graph(&physical)?
                .map(|name| {
                    let lease = input.memory.reserve(name.capacity())?;
                    Ok::<_, StorageBackendError>(Budgeted::new(name, lease))
                })
                .transpose()?
        };
        let Some((key, graph)) = path_index::registration_from(
            &physical,
            recorded.as_ref().map(|owner| owner.as_str()),
            self.graphs,
            input,
        )
        .await?
        else {
            return Err(StorageBackendError::Other(format!(
                "invalid path-index key `{}`",
                physical.as_str()
            )));
        };
        drop(recorded);
        if changed.is_some_and(|names| !names.contains(graph.as_str())) {
            return Ok(());
        }
        if candidate
            .indexes
            .get_from(key.as_str(), &mut CatalogNames(input))
            .await?
            .is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "duplicate path index `{}`",
                key.as_str()
            )));
        }
        let definition = match definition {
            Some(definition) => definition,
            None => definition::read(text, input).await?,
        };
        let declaration =
            RetainedValue::with_budget(definition.bind(physical, graph), input.memory)?;
        let index = PathIndex::bind_persistent(
            self.catalog.clone(),
            self.backend.clone(),
            declaration,
            Some(input.memory),
        )?;
        let bytes = key.capacity();
        candidate
            .indexes
            .prepare_insert_from(Produced::from(key), bytes, &mut CatalogNames(input))
            .await?
            .publish(index);
        candidate.non_memory += 1;
        Ok(())
    }
}

impl PathIndexCatalog {
    /// Reconcile cold opens and selected graph refreshes through the same definition and owner checks. Materialized pairs stay in the receiving physical session.
    pub fn restored(
        &self,
        graphs: &GraphCatalog,
        catalog: &Arc<dyn CatalogFacade>,
        backend: &Arc<dyn PersistentStorageBackend>,
        changed: Option<&BTreeSet<String>>,
    ) -> StorageBackendResult<Self> {
        let mut candidate = match changed {
            None => Self::new(self.indexes.budget()),
            Some(names) => {
                let mut next = self.clone();
                for graph in names {
                    next.remove_graph(graph)?;
                }
                next
            }
        };
        let cancellation = backend.write_cancellation().unwrap_or_default();
        let bindings = Bindings {
            graphs,
            catalog,
            backend,
            read: None,
        };
        decode_image(
            &mut std::io::empty(),
            self.indexes.budget(),
            &cancellation,
            async |mut input| {
                for (physical, text) in catalog.load_path_indexes()? {
                    let lease = input.memory.reserve(physical.capacity())?;
                    bindings
                        .bind(
                            &mut candidate,
                            Budgeted::new(physical, lease),
                            &text,
                            changed,
                            &mut input,
                        )
                        .await?;
                }
                Ok(candidate)
            },
        )
    }

    /// Remove only registrations of this graph, including encoded names and ordinary keys.
    pub fn remove_graph(&mut self, graph: &str) -> StorageBackendResult<()> {
        let (ordinary, encoded) = path_index::prefixes(graph);
        let mut candidate = self.clone();
        if let Some(prefix) = ordinary {
            candidate.remove_prefix(&prefix)?;
        }
        // A literal graph name may start with another graph's encoded prefix.
        // Its ordinary pair still has a literal separator, excluded from every encoded key.
        candidate.remove_prefix_if(&encoded, |key| !key.contains("::"))?;
        self.publish(candidate);
        Ok(())
    }
}
