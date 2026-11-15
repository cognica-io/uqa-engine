//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete stored text precedes private scoring-catalog publication.

use super::ScoringParameters;
use crate::{
    catalog::{input::CatalogInputLimits, ordering::compare_names, text_input::CatalogTextFamily},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};
use std::{cmp::Ordering, sync::Arc};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MapKeyInput, MemoryBudget},
};

pub struct ScoringParameterCatalogInput {
    input: ImageDecoder<ScoringParameters, StorageBackendError>,
}

impl ScoringParameterCatalogInput {
    /// The caller retains the selected catalog transaction. Preserve complete physical decoding, provider row order and last-definition replacement before publishing one native root. Text stays opaque and its original allocations transfer to the receiving catalog.
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
                let mut source = catalog.begin_text_load(
                    CatalogTextFamily::ScoringParameters,
                    limits,
                    &retained,
                )?;
                let rows = loop {
                    work.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop((source, catalog));
                let mut result = ScoringParameters::new(retained.memory());
                let (rows, _row_memory) = rows.into_parts();
                for row in rows {
                    work.checkpoint().await?;
                    let (row, memory) = row.into_budgeted().into_parts();
                    result = result
                        .insert_restored_from(row.name, Budgeted::new(row.value, memory), &mut work)
                        .await?;
                }
                retained.check()?;
                Ok(result.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<ScoringParameters>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

impl ScoringParameters {
    /// Both persistent and native image input transfer the original name/value reservation. Complete retained comparisons and path admission precede infallible publication; failures consume only this private candidate.
    pub(super) async fn insert_restored_from(
        mut self,
        name: String,
        encoded: Budgeted<String>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        input.checkpoint().await?;
        let bytes = encoded.reserved_bytes();
        self.values
            .prepare_insert_from(name, bytes, &mut Names(input))
            .await?
            .publish(encoded);
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
