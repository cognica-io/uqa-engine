//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical model rows precede typed decoding and private cache publication.

use super::ModelCatalog;
use crate::{model_image::ModelRetention, model_json::read_model_json_from, DeepModel};
use std::{cmp::Ordering, sync::Arc};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MapKeyInput, MemoryBudget},
};
use uqa_storage::{
    catalog::{compare_names, input::CatalogInputLimits, text_input::CatalogTextFamily},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

pub struct ModelCatalogInput {
    input: ImageDecoder<ModelCatalog, StorageBackendError>,
}

impl ModelCatalogInput {
    /// The caller retains the catalog transaction. Physical input, JSON conversion, weight admission and retained map insertion resume through one allowance; only the complete model root is exposed.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                work.checkpoint().await?;
                let mut source =
                    catalog.begin_text_load(CatalogTextFamily::Models, limits, &retained)?;
                let rows = loop {
                    work.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop((source, catalog));
                let mut result = ModelCatalog::new(retained.memory());
                let (rows, _row_memory) = rows.into_parts();
                for row in rows {
                    work.checkpoint().await?;
                    let (row, mut memory) = row.into_budgeted().into_parts();
                    let name_memory = memory.split(row.name.capacity());
                    let name = Budgeted::new(row.name, name_memory);
                    let text = Budgeted::new(row.value, memory);
                    let model = read_model_json_from(&text, &mut work).await?;
                    drop(text);
                    result = result.insert_restored_from(name, model, &mut work).await?;
                }
                retained.check()?;
                Ok(result.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<ModelCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

impl ModelCatalog {
    async fn insert_restored_from(
        mut self,
        name: Budgeted<String>,
        model: Budgeted<DeepModel>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let control = StorageReadControl::new(input.memory, input.cancellation);
        let bytes = {
            let mut retention = ModelRetention::new(&model)?;
            loop {
                input.checkpoint().await?;
                if let Some(bytes) = retention.advance(|| control.check())? {
                    break bytes;
                }
            }
        };
        let (name, source, bytes) = super::input::source(name, model, bytes)?;
        self.values
            .prepare_insert_from(name, bytes, &mut Names(input))
            .await?
            .publish(source);
        input.checkpoint().await?;
        Ok(self)
    }
}

struct Names<'a, 'c>(&'a mut AsyncImageInput<'c>);

impl MapKeyInput<String> for Names<'_, '_> {
    type Error = StorageBackendError;

    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        self.0.checkpoint().await.map_err(Into::into)
    }

    async fn compare(&mut self, left: &String, right: &String) -> StorageBackendResult<Ordering> {
        compare_names(left, right, self.0).await
    }
}

#[cfg(test)]
mod tests;
