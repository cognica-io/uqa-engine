//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed sequence images retain both original owners while their ordered membership is checked.

use super::{invalid, SequenceCatalog, SequenceSecurities};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::SQLError;

pub(super) fn start(
    catalog: SequenceCatalog,
    security: SequenceSecurities,
    mutations: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ImageDecoder<SequenceCatalog, SQLError>, SQLError> {
    let memory = catalog.budget().clone();
    let mutations = mutations.clone();
    Ok(ImageDecoder::new(
        &memory,
        cancellation,
        |mut reader| async move { read(catalog, security, &mutations, reader.input()).await },
    )?)
}

pub(super) async fn read(
    catalog: SequenceCatalog,
    security: SequenceSecurities,
    mutations: &MemoryBudget,
    mut input: AsyncImageInput<'_>,
) -> Result<SequenceCatalog, SQLError> {
    if security.len() != catalog.len() {
        return Err(invalid("authority membership count"));
    }
    for (relation, record) in catalog.iter() {
        input.checkpoint().await?;
        if security.persistence(relation) != Some(record.persistence) {
            return Err(invalid("authority membership or persistence"));
        }
    }
    input.cancellation.check()?;
    Ok(SequenceCatalog {
        security,
        ..catalog
    }
    .into_mutation_budget(mutations))
}
